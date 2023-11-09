use color_eyre::eyre::Context;
use color_eyre::Result;
use launch_pad::process::Process;
use launch_pad::ProcessKey;
use std::os::fd::{IntoRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tracing::Instrument;

use crate::comp::create_privileged_socket;

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
	fd: RawFd,
	restart_span: tracing::Span,
	restart_cmd: &'static str,
	restart_key: Arc<Mutex<Option<ProcessKey>>>,
	restart_env_vars: Vec<(String, String)>,
	restart_fd: RawFd,
	socket_tx: mpsc::UnboundedSender<Vec<tokio::net::UnixStream>>,
) -> Process {
	env_vars.retain(|v| &v.0 != "WAYLAND_SOCKET");

	let stdout_span = span.clone();
	let stderr_span = span.clone();
	let mut sockets = Vec::with_capacity(1);
	let (env_vars, privileged_fd) = create_privileged_socket(&mut sockets, &env_vars).unwrap();
	_ = socket_tx.send(sockets);
	let env_clone = env_vars.clone();
	let socket_tx_clone = socket_tx.clone();
	let privileged_fd = privileged_fd.into_raw_fd();
	Process::new()
		.with_executable(cmd)
		.with_fds(move || vec![privileged_fd, fd])
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
			let new_process = notifications_process(
				restart_span.clone(),
				restart_cmd,
				restart_key.clone(),
				restart_env_vars.clone(),
				restart_fd,
				span.clone(),
				cmd,
				key.clone(),
				env_clone.clone(),
				fd,
				socket_tx_clone.clone(),
			);
			let restart_key = restart_key.clone();
			let socket_tx_clone = socket_tx_clone.clone();

			let env_clone = env_clone.clone();
			let mut pman_clone = pman.clone();
			async move {
				if will_restart {
					let mut sockets = Vec::with_capacity(1);
					let (env_vars, new_fd) =
						create_privileged_socket(&mut sockets, &env_clone).unwrap();

					let new_fd = new_fd.into_raw_fd();

					if let Err(why) = socket_tx_clone.send(sockets) {
						error!(?why, "Failed to send the privileged socket");
					}
					if let Err(why) = pman_clone.update_process_env(&my_key, env_vars).await {
						error!(?why, "Failed to update environment variables");
					}
					if let Err(why) = pman_clone
						.update_process_fds(&my_key, move || vec![new_fd, fd])
						.await
					{
						error!(?why, "Failed to update fds");
					}

					let Some(old) = restart_key.lock().unwrap().clone() else {
						error!("Couldn't stop previous invocation of {}", cmd);
						return;
					};
					_ = pman.stop_process(old).await;

					if let Ok(new) = pman.start(new_process).await {
						let mut guard = restart_key.lock().unwrap();
						*guard = Some(new);
					}
				}
			}
		})
		.with_env(env_vars)
}
