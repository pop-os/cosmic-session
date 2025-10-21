use color_eyre::{Result, eyre::Context};
use cosmic_notifications_util::{DAEMON_NOTIFICATIONS_FD, PANEL_NOTIFICATIONS_FD};
use launch_pad::{ProcessKey, process::Process};
use rustix::fd::AsRawFd;
use std::{
	os::{fd::OwnedFd, unix::net::UnixStream},
	sync::Arc,
};
use tokio::sync::Mutex;
use tracing::Instrument;

pub fn create_socket() -> Result<(OwnedFd, OwnedFd)> {
	// Create a new pair of unnamed Unix sockets
	let (sock_1, sock_2) = UnixStream::pair().wrap_err("failed to create socket pair")?;

	// Turn the sockets into non-blocking fd, which we can pass to the child
	// process
	sock_1
		.set_nonblocking(true)
		.wrap_err("failed to mark client socket as non-blocking")?;

	sock_2
		.set_nonblocking(true)
		.wrap_err("failed to mark client socket as non-blocking")?;

	Ok((OwnedFd::from(sock_1), OwnedFd::from(sock_2)))
}

pub fn notifications_process(
	span: tracing::Span,
	cmd: &'static str,
	key: Arc<Mutex<Option<ProcessKey>>>,
	mut env_vars: Vec<(String, String)>,
	fd: OwnedFd,
	restart_span: tracing::Span,
	restart_cmd: &'static str,
	restart_key: Arc<Mutex<Option<ProcessKey>>>,
	restart_env_vars: Vec<(String, String)>,
) -> Process {
	env_vars.retain(|v| &v.0 != "WAYLAND_SOCKET");

	let stdout_span = span.clone();
	let stderr_span = span.clone();
	let env_clone = env_vars.clone();
	Process::new()
		.with_executable(cmd)
		.with_fds(move || vec![fd])
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
		.with_on_exit(move |pman, my_key, _, will_restart| {
			// force restart of notifications / panel when the other exits
			// also update the environment variables to use the new socket
			let (my_fd, their_fd) = create_socket().expect("Failed to create notification socket");
			let mut my_env_vars = env_clone.clone();
			if let Some((_k, v)) = my_env_vars
				.iter_mut()
				.find(|(k, _v)| k == PANEL_NOTIFICATIONS_FD || k == DAEMON_NOTIFICATIONS_FD)
			{
				*v = my_fd.as_raw_fd().to_string();
			}

			let mut their_env_vars = restart_env_vars.clone();
			if let Some((_k, v)) = their_env_vars
				.iter_mut()
				.find(|(k, _v)| k == PANEL_NOTIFICATIONS_FD || k == DAEMON_NOTIFICATIONS_FD)
			{
				*v = their_fd.as_raw_fd().to_string();
			}

			let new_process = notifications_process(
				restart_span.clone(),
				restart_cmd,
				restart_key.clone(),
				their_env_vars.clone(),
				their_fd,
				span.clone(),
				cmd,
				key.clone(),
				my_env_vars.clone(),
			);
			let restart_key = restart_key.clone();

			let mut pman_clone = pman.clone();
			async move {
				if will_restart {
					if let Err(why) = pman_clone.update_process_env(&my_key, my_env_vars).await {
						error!(?why, "Failed to update environment variables");
					}
					if let Err(why) = pman_clone
						.update_process_fds(&my_key, move || vec![my_fd])
						.await
					{
						error!(?why, "Failed to update fds");
					}

					let Some(old) = *restart_key.lock().await else {
						error!("Couldn't stop previous invocation of {}", cmd);
						return;
					};
					_ = pman.stop_process(old).await;

					if let Ok(new) = pman.start(new_process).await {
						let mut guard = restart_key.lock().await;
						*guard = Some(new);
					}
				}
			}
		})
		.with_env(env_vars)
}
