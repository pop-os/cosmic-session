// SPDX-License-Identifier: GPL-3.0-only
use std::{
	future::Future,
	process::{ExitStatus, Stdio},
};
use tokio::{
	io::{AsyncBufReadExt, BufReader},
	process::Command,
};
use tokio_util::sync::CancellationToken;

pub enum ProcessEvent {
	Started,
	Stdout(String),
	Stderr(String),
	Ended { status: ExitStatus },
}

pub struct ProcessHandler<AsyncTask, Task>
where
	AsyncTask: Future<Output = ()> + Send,
	Task: Fn(ProcessEvent) -> AsyncTask + Send + 'static,
{
	task: Task,
	cancellation_token: CancellationToken,
}

impl<AsyncTask, Task> ProcessHandler<AsyncTask, Task>
where
	AsyncTask: Future<Output = ()> + Send,
	Task: Fn(ProcessEvent) -> AsyncTask + Send + 'static,
{
	pub fn new(task: Task, cancellation_token: &CancellationToken) -> Self {
		Self {
			task,
			cancellation_token: cancellation_token.child_token(),
		}
	}

	pub fn run(self, executable: impl ToString, args: Vec<String>) {
		let executable = executable.to_string();
		tokio::spawn(async move {
			let mut child = match Command::new(&executable)
				.args(&args)
				.stdin(Stdio::null())
				.stdout(Stdio::piped())
				.stderr(Stdio::piped())
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
			let mut stdout = BufReader::new(child.stdout.take().unwrap()).lines();
			let mut stderr = BufReader::new(child.stderr.take().unwrap()).lines();
			let task = (self.task)(ProcessEvent::Started);
			task.await;
			loop {
				tokio::select! {
					status = child.wait() => match status {
						Ok(status) => {
							info!("'{}' exited with status {}", executable, status);
							let task = (self.task)(ProcessEvent::Ended { status });
							task.await;
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
							let task = (self.task)(ProcessEvent::Stdout(line));
							task.await;
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
							let task = (self.task)(ProcessEvent::Stderr(line));
							task.await;
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
						return;
					}
				}
			}
		});
	}
}
