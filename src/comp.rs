// SPDX-License-Identifier: GPL-3.0-only
use crate::process::{ProcessEvent, ProcessHandler};
use tokio::sync::{mpsc::unbounded_channel, oneshot};
use tokio_util::sync::CancellationToken;

pub async fn run_compositor(token: CancellationToken, wayland_display_tx: oneshot::Sender<String>) {
	let mut wayland_display_tx = Some(wayland_display_tx);
	let (tx, mut rx) = unbounded_channel::<ProcessEvent>();
	ProcessHandler::new(tx, &token).run("cosmic-comp", vec![], vec![]);
	while let Some(event) = rx.recv().await {
		match event {
			ProcessEvent::Started => {
				info!("started");
			}
			// cosmic-comp outputs everything to stderr because slog
			ProcessEvent::Stdout(line) | ProcessEvent::Stderr(line) => {
				if line.contains("Listening on \"") {
					// Message format: Listening on "wayland-0"
					if let Some(tx) = wayland_display_tx.take() {
						let socket_name = line
							.split('"')
							.nth(1)
							.expect("failed to get WAYLAND_DISPLAY");
						tx.send(socket_name.to_string())
							.expect("failed to send WAYLAND_DISPLAY back to main app");
					}
				}
				info!("{}", line);
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
