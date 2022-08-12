// SPDX-License-Identifier: GPL-3.0-only
use crate::process::{ProcessEvent, ProcessHandler};
use std::os::unix::io::OwnedFd;
use tokio::sync::mpsc::unbounded_channel;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, Span};

pub fn run_executable(
	token: CancellationToken,
	span: Span,
	executable: &'static str,
	args: Vec<String>,
	env_vars: Vec<(String, String)>,
	fds: Vec<OwnedFd>,
) {
	let span_2 = span.clone();
	let (tx, mut rx) = unbounded_channel::<ProcessEvent>();
	tokio::spawn(
		async move {
			ProcessHandler::new(tx, &token).run(executable, args, env_vars, fds, &span);
			while let Some(event) = rx.recv().await {
				match event {
					ProcessEvent::Started => {
						info!("started");
					}
					ProcessEvent::Stdout(line) => {
						info!("{}", line);
					}
					ProcessEvent::Stderr(line) => {
						error!("{}", line);
					}
					ProcessEvent::Ended(Some(status)) => {
						error!("exited with status {}", status);
						return;
					}
					ProcessEvent::Ended(None) => {
						error!("exited");
						return;
					}
				}
			}
		}
		.instrument(span_2),
	);
}
