// SPDX-License-Identifier: GPL-3.0-only
#[macro_use]
extern crate tracing;

mod a11y;
mod comp;
mod notifications;
mod process;
mod service;
mod systemd;

use async_signals::Signals;
use color_eyre::{eyre::WrapErr, Result};
use comp::create_privileged_socket;
use cosmic_notifications_util::{DAEMON_NOTIFICATIONS_FD, PANEL_NOTIFICATIONS_FD};
use futures_util::StreamExt;
#[cfg(feature = "autostart")]
use itertools::Itertools;
use launch_pad::{process::Process, ProcessManager};
use service::SessionRequest;
#[cfg(feature = "autostart")]
use std::collections::HashSet;
#[cfg(feature = "autostart")]
use std::path::PathBuf;
#[cfg(feature = "autostart")]
use std::process::{Command, Stdio};
use std::{
	borrow::Cow,
	env,
	os::fd::{AsRawFd, OwnedFd},
	sync::Arc,
};
#[cfg(feature = "systemd")]
use systemd::{get_systemd_env, is_systemd_used, spawn_scope};
use tokio::{
	net::UnixStream,
	sync::{
		mpsc::{self, Receiver, Sender},
		oneshot, Mutex,
	},
	time::Duration,
};
use tokio_util::sync::CancellationToken;
use tracing::{metadata::LevelFilter, Instrument};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};
use zbus::ConnectionBuilder;

use crate::notifications::notifications_process;
const XDP_COSMIC: Option<&'static str> = option_env!("XDP_COSMIC");
#[cfg(feature = "autostart")]
const AUTOSTART_DIR: &'static str = "autostart";
#[cfg(feature = "autostart")]
const ENVIRONMENT_NAME: &'static str = "COSMIC";

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
	color_eyre::install().wrap_err("failed to install color_eyre error handler")?;

	let trace = tracing_subscriber::registry();
	let env_filter = EnvFilter::builder()
		.with_default_directive(LevelFilter::INFO.into())
		.from_env_lossy();

	#[cfg(feature = "systemd")]
	if let Ok(journald) = tracing_journald::layer() {
		trace
			.with(journald)
			.with(fmt::layer())
			.with(env_filter)
			.try_init()
			.wrap_err("failed to initialize logger")?;
	} else {
		trace
			.with(fmt::layer())
			.with(env_filter)
			.try_init()
			.wrap_err("failed to initialize logger")?;
		warn!("failed to connect to journald")
	}

	#[cfg(not(feature = "systemd"))]
	trace
		.with(fmt::layer())
		.with(env_filter)
		.try_init()
		.wrap_err("failed to initialize logger")?;

	log_panics::init();

	let (session_tx, mut session_rx) = tokio::sync::mpsc::channel(10);
	let session_tx_clone = session_tx.clone();
	let _conn = ConnectionBuilder::session()?
		.name("com.system76.CosmicSession")?
		.serve_at(
			"/com/system76/CosmicSession",
			service::SessionService { session_tx },
		)?
		.build()
		.await?;

	loop {
		match start(session_tx_clone.clone(), &mut session_rx).await {
			Ok(Status::Exited) => {
				info!("Exited cleanly");
				break;
			}
			Ok(Status::Restarted) => {
				info!("Restarting");
			}
			Err(error) => {
				error!("Restarting after error: {:?}", error);
			}
		};
		// Drain the session channel.
		while session_rx.try_recv().is_ok() {}
	}
	Ok(())
}

#[derive(Debug)]
pub enum Status {
	Restarted,
	Exited,
}

async fn start(
	session_tx: Sender<SessionRequest>,
	session_rx: &mut Receiver<SessionRequest>,
) -> Result<Status> {
	info!("Starting cosmic-session");

	let mut args = env::args().skip(1);
	let (executable, args) = (
		args.next().unwrap_or_else(|| String::from("cosmic-comp")),
		args.collect::<Vec<_>>(),
	);

	let process_manager = ProcessManager::new().await;
	_ = process_manager.set_max_restarts(usize::MAX).await;
	_ = process_manager
		.set_restart_mode(launch_pad::RestartMode::ExponentialBackoff(
			Duration::from_millis(10),
		))
		.await;
	let token = CancellationToken::new();
	let (socket_tx, socket_rx) = mpsc::unbounded_channel();
	let (env_tx, env_rx) = oneshot::channel();
	let compositor_handle = comp::run_compositor(
		&process_manager,
		executable.clone(),
		args,
		token.child_token(),
		socket_rx,
		env_tx,
		session_tx,
	)
	.wrap_err("failed to start compositor")?;

	let mut env_vars = env_rx
		.await
		.expect("failed to receive environmental variables")
		.into_iter()
		.collect::<Vec<_>>();
	info!(
		"got environmental variables from cosmic-comp: {:?}",
		env_vars
	);

	// now that cosmic-comp is ready, set XDG_SESSION_TYPE=wayland for new processes
	std::env::set_var("XDG_SESSION_TYPE", "wayland");
	env_vars.push(("XDG_SESSION_TYPE".to_string(), "wayland".to_string()));
	systemd::set_systemd_environment("XDG_SESSION_TYPE", "wayland").await;

	#[cfg(feature = "systemd")]
	if *is_systemd_used() {
		match get_systemd_env().await {
			Ok(env) => {
				for systemd_env in env {
					// Only update the envvar if unset
					if std::env::var_os(&systemd_env.key) == None {
						// Blacklist of envvars that we shouldn't touch (taken from KDE)
						if (!systemd_env.key.starts_with("XDG_")
							|| systemd_env.key == "XDG_DATA_DIRS"
							|| systemd_env.key == "XDG_CONFIG_DIRS")
							&& systemd_env.key != "DISPLAY"
							&& systemd_env.key != "XAUTHORITY"
							&& systemd_env.key != "WAYLAND_DISPLAY"
							&& systemd_env.key != "WAYLAND_SOCKET"
							&& systemd_env.key != "_"
							&& systemd_env.key != "SHELL"
							&& systemd_env.key != "SHLVL"
						{
							std::env::set_var(systemd_env.key, systemd_env.value);
						}
					}
				}
			}
			Err(err) => {
				warn!("Failed to sync systemd environment {}.", err);
			}
		}
	}

	let stdout_span = info_span!(parent: None, "cosmic-settings-daemon");
	let stderr_span = stdout_span.clone();
	let (settings_exit_tx, settings_exit_rx) = oneshot::channel();
	let settings_exit_tx = Arc::new(std::sync::Mutex::new(Some(settings_exit_tx)));
	let settings_daemon = process_manager
		.start(
			Process::new()
				.with_executable("cosmic-settings-daemon")
				.with_on_stdout(move |_, _, line| {
					let stdout_span = stdout_span.clone();
					async move {
						info!("{}", line);
					}
					.instrument(stdout_span)
				})
				.with_on_stderr(move |_, _, line| {
					let stderr_span = stderr_span.clone();
					async move {
						warn!("{}", line);
					}
					.instrument(stderr_span)
				})
				.with_on_exit(move |_, _, _, will_restart| {
					if !will_restart {
						if let Some(tx) = settings_exit_tx.lock().unwrap().take() {
							_ = tx.send(());
						}
					}
					async {}
				}),
		)
		.await
		.expect("failed to start settings daemon");

	// notifying the user service manager that we've reached the
	// graphical-session.target, which should only happen after:
	// - cosmic-comp is ready
	// - we've set any related variables
	// - cosmic-settings-daemon is ready
	systemd::start_systemd_target().await;
	// Always stop the target when the process exits or panics.
	scopeguard::defer! {
		systemd::stop_systemd_target();
	}

	// start a11y if configured
	tokio::spawn(a11y::start_a11y(env_vars.clone(), process_manager.clone()));

	let (panel_notifications_fd, daemon_notifications_fd) =
		notifications::create_socket().expect("Failed to create notification socket");

	let mut daemon_env_vars = env_vars.clone();
	daemon_env_vars.push((
		DAEMON_NOTIFICATIONS_FD.to_string(),
		daemon_notifications_fd.as_raw_fd().to_string(),
	));
	let mut panel_env_vars = env_vars.clone();
	panel_env_vars.push((
		PANEL_NOTIFICATIONS_FD.to_string(),
		panel_notifications_fd.as_raw_fd().to_string(),
	));

	let panel_key = Arc::new(Mutex::new(None));
	let notif_key = Arc::new(Mutex::new(None));

	let notifications_span = info_span!(parent: None, "cosmic-notifications");
	let panel_span = info_span!(parent: None, "cosmic-panel");

	let mut guard = notif_key.lock().await;
	*guard = Some(
		process_manager
			.start(notifications_process(
				notifications_span.clone(),
				"cosmic-notifications",
				notif_key.clone(),
				daemon_env_vars.clone(),
				daemon_notifications_fd,
				panel_span.clone(),
				"cosmic-panel",
				panel_key.clone(),
				panel_env_vars.clone(),
				socket_tx.clone(),
			))
			.await
			.expect("failed to start notifications daemon"),
	);
	drop(guard);

	let mut guard = panel_key.lock().await;
	*guard = Some(
		process_manager
			.start(notifications_process(
				panel_span,
				"cosmic-panel",
				panel_key.clone(),
				panel_env_vars,
				panel_notifications_fd,
				notifications_span,
				"cosmic-notifications",
				notif_key,
				daemon_env_vars,
				socket_tx.clone(),
			))
			.await
			.expect("failed to start panel"),
	);
	drop(guard);

	let span = info_span!(parent: None, "cosmic-app-library");
	start_component(
		"cosmic-app-library",
		span,
		&process_manager,
		&env_vars,
		&socket_tx,
		Vec::new(),
	)
	.await;

	let span = info_span!(parent: None, "cosmic-launcher");
	start_component(
		"cosmic-launcher",
		span,
		&process_manager,
		&env_vars,
		&socket_tx,
		Vec::new(),
	)
	.await;

	let span = info_span!(parent: None, "cosmic-workspaces");
	start_component(
		"cosmic-workspaces",
		span,
		&process_manager,
		&env_vars,
		&socket_tx,
		Vec::new(),
	)
	.await;

	let span = info_span!(parent: None, "cosmic-osd");
	start_component(
		"cosmic-osd",
		span,
		&process_manager,
		&env_vars,
		&socket_tx,
		Vec::new(),
	)
	.await;

	let span = info_span!(parent: None, "cosmic-bg");
	start_component(
		"cosmic-bg",
		span,
		&process_manager,
		&env_vars,
		&socket_tx,
		Vec::new(),
	)
	.await;

	let span = info_span!(parent: None, "cosmic-greeter");
	start_component(
		"cosmic-greeter",
		span,
		&process_manager,
		&env_vars,
		&socket_tx,
		Vec::new(),
	)
	.await;

	let span = info_span!(parent: None, "cosmic-files-applet");
	start_component(
		"cosmic-files-applet",
		span,
		&process_manager,
		&env_vars,
		&socket_tx,
		Vec::new(),
	)
	.await;

	let span = info_span!(parent: None, "cosmic-idle");
	start_component(
		"cosmic-idle",
		span,
		&process_manager,
		&env_vars,
		&socket_tx,
		Vec::new(),
	)
	.await;

	if env::var("XDG_CURRENT_DESKTOP").as_deref() == Ok("COSMIC") {
		let span = info_span!(parent: None, "xdg-desktop-portal-cosmic");
		let mut sockets = Vec::with_capacity(1);
		let extra_env = Vec::with_capacity(1);
		let portal_extras =
			if let Ok((mut env, fd)) = create_privileged_socket(&mut sockets, &extra_env) {
				let mut env = env.remove(0);
				env.0 = "PORTAL_WAYLAND_SOCKET".to_string();
				vec![(fd, env, sockets.remove(0))]
			} else {
				Vec::new()
			};
		start_component(
			XDP_COSMIC.unwrap_or("/usr/libexec/xdg-desktop-portal-cosmic"),
			span,
			&process_manager,
			&env_vars,
			&socket_tx,
			portal_extras,
		)
		.await;
	}

	#[cfg(feature = "autostart")]
	if !*is_systemd_used() {
		info!("looking for autostart folders");
		let mut directories_to_scan = Vec::new();

		// we start by taking user specific directories, so that we can deduplicate and ensure
		// user overrides are respected

		// user specific directories
		if let Some(user_config_dir) = dirs::config_dir() {
			directories_to_scan.push(user_config_dir.join(AUTOSTART_DIR));
		}

		// system-wide directories
		if let Some(xdg_config_dirs) = env::var_os("XDG_CONFIG_DIRS") {
			let xdg_config_dirs = xdg_config_dirs
				.into_string()
				.expect("Invalid XDG_CONFIG_DIRS");
			let dir_list = xdg_config_dirs.split(":");

			for dir in dir_list {
				directories_to_scan.push(PathBuf::from(dir).join(AUTOSTART_DIR));
			}
		} else {
			directories_to_scan.push(PathBuf::from("/etc/xdg/").join(AUTOSTART_DIR));
		}

		info!("found autostart folders: {:?}", directories_to_scan);

		let mut dedupe = HashSet::new();

		let iter = freedesktop_desktop_entry::Iter::new(directories_to_scan.into_iter());
		let autostart_env = env_vars.clone();
		for entry in iter.entries::<&str>(None) {
			// we've already tried to execute this!
			if dedupe.contains(&entry.appid) {
				continue;
			}

			// skip if we have an OnlyShowIn entry that doesn't include COSMIC
			if let Some(only_show_in) = entry.only_show_in() {
				if !only_show_in.contains(&ENVIRONMENT_NAME) {
					continue;
				}
			}

			// ... OR we have a NotShowIn entry that includes COSMIC
			if let Some(not_show_in) = entry.not_show_in() {
				if not_show_in.contains(&ENVIRONMENT_NAME) {
					continue;
				}
			}

			info!(
				"trying to start appid {} ({})",
				entry.appid,
				entry.path.display()
			);

			if let Some(exec_raw) = entry.exec() {
				let mut exec_words = exec_raw.split(" ");

				if let Some(program_name) = exec_words.next() {
					// filter out any placeholder args, since we might not be able to deal with them
					let filtered_args = exec_words.filter(|s| !s.starts_with("%")).collect_vec();

					// escape them
					let escaped_args = shell_words::split(&*filtered_args.join(" "));
					if let Ok(args) = escaped_args {
						info!("trying to start {} {}", program_name, args.join(" "));

						let mut command = Command::new(program_name);
						command.args(args);

						// add relevant envs
						for (k, v) in &autostart_env {
							command.env(k, v);
						}

						// detach stdin/out/err (should we?)
						let child = command
							.stdin(Stdio::null())
							.stdout(Stdio::null())
							.stderr(Stdio::null())
							.spawn();

						if let Ok(child) = child {
							info!(
								"successfully started program {} {}",
								entry.appid,
								child.id()
							);
							dedupe.insert(entry.appid);
						} else {
							info!("could not start program {}", entry.appid);
						}
					} else {
						let why = escaped_args.unwrap_err();
						error!(?why, "could not parse arguments");
					}
				}
			}
		}
		info!("started {} programs", dedupe.len());
	}

	let mut signals = Signals::new(vec![libc::SIGTERM, libc::SIGINT]).unwrap();
	let mut status = Status::Exited;
	loop {
		let session_dbus_rx_next = session_rx.recv();
		tokio::select! {
			res = session_dbus_rx_next => {
				match res {
					Some(service::SessionRequest::Exit) => {
						info!("EXITING: session exited by request");
						break;
					}
					Some(service::SessionRequest::Restart) => {
						info!("RESTARTING: session restarted by request");
						status = Status::Restarted;
						break;
					}
					None => {
						warn!("exit channel dropped session");
						break;
					}
				}
			},
			signal = signals.next() => match signal {
				Some(libc::SIGTERM | libc::SIGINT) => {
					info!("EXITING: received request to terminate");
					break;
				}
				Some(signal) => unreachable!("EXITING: received unhandled signal {}", signal),
				None => break,
			}
		}
	}
	compositor_handle.abort();
	token.cancel();
	if let Err(err) = process_manager.stop_process(settings_daemon).await {
		tracing::error!("Failed to gracefully stop settings daemon. {err:?}");
	} else {
		match tokio::time::timeout(Duration::from_secs(1), settings_exit_rx).await {
			Ok(Ok(_)) => {}
			_ => {
				tracing::error!("Failed to gracefully stop settings daemon.");
			}
		};
	};

	tokio::time::sleep(std::time::Duration::from_secs(2)).await;
	Ok(status)
}

async fn start_component(
	cmd: impl Into<Cow<'static, str>>,
	span: tracing::Span,
	process_manager: &ProcessManager,
	env_vars: &[(String, String)],
	socket_tx: &mpsc::UnboundedSender<Vec<UnixStream>>,
	extra_fds: Vec<(OwnedFd, (String, String), UnixStream)>,
) {
	let mut sockets = Vec::with_capacity(2);
	let (mut env_vars, fd) = create_privileged_socket(&mut sockets, &env_vars).unwrap();

	let socket_tx_clone = socket_tx.clone();
	let stdout_span = span.clone();
	let stderr_span = span.clone();
	let cmd = cmd.into();
	let cmd_clone = cmd.clone();

	let (mut fds, extra_fd_env, mut streams): (Vec<_>, Vec<_>, Vec<_>) =
		itertools::multiunzip(extra_fds);
	for kv in &extra_fd_env {
		env_vars.push(kv.clone());
	}

	sockets.append(&mut streams);
	if let Err(why) = socket_tx.send(sockets) {
		error!(?why, "Failed to send the privileged socket");
	}
	let (extra_fd_env, _): (Vec<_>, Vec<_>) = extra_fd_env.into_iter().unzip();
	fds.push(fd);
	process_manager
		.start(
			Process::new()
				.with_executable(cmd.clone())
				.with_env(env_vars.iter().cloned())
				.with_on_stdout(move |_, _, line| {
					let stdout_span = stdout_span.clone();
					async move {
						info!("{}", line);
					}
					.instrument(stdout_span)
				})
				.with_on_stderr(move |_, _, line| {
					let stderr_span = stderr_span.clone();
					async move {
						warn!("{}", line);
					}
					.instrument(stderr_span)
				})
				.with_on_start(move |pman, pkey, _will_restart| async move {
					#[cfg(feature = "systemd")]
					if *is_systemd_used() {
						if let Ok((innr_cmd, Some(pid))) = pman.get_exe_and_pid(pkey).await {
							if let Err(err) = spawn_scope(innr_cmd.clone(), vec![pid]).await {
								warn!(
									"Failed to spawn scope for {}. Creating transient unit failed \
									 with {}",
									innr_cmd, err
								);
							};
						}
					}
				})
				.with_on_exit(move |mut pman, key, err_code, will_restart| {
					if let Some(err) = err_code {
						error!("{cmd_clone} exited with error {}", err.to_string());
					}
					let extra_fd_env = extra_fd_env.clone();
					let socket_tx_clone = socket_tx_clone.clone();
					async move {
						if !will_restart {
							return;
						}

						let mut sockets = Vec::with_capacity(1 + extra_fd_env.len());
						let mut fds = Vec::with_capacity(1 + extra_fd_env.len());
						let (mut env_vars, fd) =
							create_privileged_socket(&mut sockets, &[]).unwrap();
						fds.push(fd);
						for k in extra_fd_env {
							let (mut fd_env_vars, fd) =
								create_privileged_socket(&mut sockets, &[]).unwrap();
							fd_env_vars.last_mut().unwrap().0 = k;
							env_vars.append(&mut fd_env_vars);
							fds.push(fd)
						}

						if let Err(why) = socket_tx_clone.send(sockets) {
							error!(?why, "Failed to send the privileged socket");
						}
						if let Err(why) = pman.update_process_env(&key, env_vars).await {
							error!(?why, "Failed to update environment variables");
						}
						if let Err(why) = pman.update_process_fds(&key, move || fds).await {
							error!(?why, "Failed to update fds");
						}
					}
				})
				.with_fds(move || fds),
		)
		.await
		.unwrap_or_else(|_| panic!("failed to start {}", cmd));
}
