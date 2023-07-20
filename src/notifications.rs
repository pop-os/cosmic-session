use color_eyre::eyre::Context;
use color_eyre::Result;
use launch_pad::process::Process;
use launch_pad::ProcessKey;
use std::os::fd::{OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
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
	env_vars: Vec<(String, String)>,
	fd: RawFd,
	restart_span: tracing::Span,
	restart_cmd: &'static str,
	restart_key: Arc<Mutex<Option<ProcessKey>>>,
	restart_env_vars: Vec<(String, String)>,
	restart_fd: RawFd,
) -> Process {
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
		.with_on_exit(move |pman, _, _, will_restart| {
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
			);
			let restart_key = restart_key.clone();
			async move {
				if will_restart {
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
