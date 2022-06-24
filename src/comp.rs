use crate::process::{ProcessEvent, ProcessHandler};
use tokio::sync::mpsc::unbounded_channel;
use tokio_util::sync::CancellationToken;

pub async fn run_compositor(token: CancellationToken) {
	let (tx, mut rx) = unbounded_channel::<ProcessEvent>();
	ProcessHandler::new(tx, &token).run("cosmic-comp", vec![]);
	let span = info_span!("cosmic-comp");
	let _enter = span.enter();
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
