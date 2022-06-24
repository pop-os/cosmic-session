// SPDX-License-Identifier: GPL-3.0-only
use crate::process::{ProcessEvent, ProcessHandler};
use tokio::sync::mpsc::unbounded_channel;
use tokio_util::sync::CancellationToken;

pub async fn run_panel(token: CancellationToken, wayland_socket: String) {
	let (tx, mut rx) = unbounded_channel::<ProcessEvent>();
	ProcessHandler::new(tx, &token).run("cosmic-panel", vec![], vec![(
		"WAYLAND_SOCKET".into(),
		wayland_socket,
	)]);
	let span = info_span!("cosmic-panel");
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
