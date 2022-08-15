// SPDX-License-Identifier: GPL-3.0-only
use color_eyre::eyre::{ContextCompat, Result, WrapErr};
use nix::fcntl;
use std::{
	os::unix::prelude::*,
	process::{ExitStatus, Stdio},
};
use tokio::{
	io::{AsyncBufReadExt, BufReader},
	process::Command,
	sync::mpsc::UnboundedSender,
};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, Span};

pub enum ProcessEvent {
	Started,
	Stdout(String),
	Stderr(String),
	Ended(Option<ExitStatus>),
}

pub struct ProcessHandler {
	tx: UnboundedSender<ProcessEvent>,
	cancellation_token: CancellationToken,
}

impl ProcessHandler {
	pub fn new(tx: UnboundedSender<ProcessEvent>, cancellation_token: &CancellationToken) -> Self {
		Self {
			tx,
			cancellation_token: cancellation_token.child_token(),
		}
	}

	// TODO: Use `OwnedFd` when stable
	pub fn run(
		self,
		executable: impl ToString,
		args: Vec<String>,
		vars: Vec<(String, String)>,
		fds: Vec<OwnedFd>,
		span: &Span,
	) {
		let executable = executable.to_string();
		tokio::spawn(
			async move {
				for fd in &fds {
					if let Err(err) = mark_as_not_cloexec(fd) {
						error!("failed to launch '{}': {}", executable, err);
						return;
					}
				}
				let mut child = match Command::new(&executable)
					.args(&args)
					.stdin(Stdio::null())
					.stdout(Stdio::piped())
					.stderr(Stdio::piped())
					.envs(vars)
					.kill_on_drop(true)
					.spawn()
				{
					Ok(child) => child,
					Err(error) => {
						error!(
							"failed to launch '{} {}': {}",
							executable,
							args.join(" "),
							error
						);
						return;
					}
				};
				drop(fds);
				let mut stdout = BufReader::new(child.stdout.take().unwrap()).lines();
				let mut stderr = BufReader::new(child.stderr.take().unwrap()).lines();
				std::mem::drop(self.tx.send(ProcessEvent::Started));
				loop {
					tokio::select! {
						status = child.wait() => match status {
							Ok(status) => {
								info!("'{}' exited with status {}", executable, status);
								std::mem::drop(self.tx.send(ProcessEvent::Ended(Some(status))));
								return;
							}
							Err(error) => {
								error!(
									"failed to wait for '{}' to end: {}",
									executable,
									error
								);
								return;
							}
						},
						line = stdout.next_line() => match line {
							Ok(Some(line)) => {
								std::mem::drop(self.tx.send(ProcessEvent::Stdout(line)));
							},
							Ok(None) => (),
							Err(error) => {
								warn!(
									"failed to get stdout line from '{}': {}",
									executable,
									error
								);
							}
						},
						line = stderr.next_line() => match line {
							Ok(Some(line)) => {
								std::mem::drop(self.tx.send(ProcessEvent::Stderr(line)));
							},
							Ok(None) => (),
							Err(error) => {
								warn!(
									"failed to get stderr line from '{}': {}",
									executable,
									error
								);
							}
						},
						_ = self.cancellation_token.cancelled() => {
							warn!("exiting '{}': cancelled", executable);
							std::mem::drop(self.tx.send(ProcessEvent::Ended(None)));
							return;
						}
					}
				}
			}
			.instrument(span.clone()),
		);
	}
}

fn mark_as_not_cloexec(file: &impl AsFd) -> Result<()> {
	let raw_fd = file.as_fd().as_raw_fd();
	let fd_flags = fcntl::FdFlag::from_bits(
		fcntl::fcntl(raw_fd, fcntl::FcntlArg::F_GETFD)
			.wrap_err("failed to get GETFD value of stream")?,
	)
	.wrap_err("failed to get fd flags from file")?;
	fcntl::fcntl(
		raw_fd,
		fcntl::FcntlArg::F_SETFD(fd_flags.difference(fcntl::FdFlag::FD_CLOEXEC)),
	)
	.wrap_err("failed to set CLOEXEC on file")?;
	Ok(())
}
