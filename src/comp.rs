use std::os::unix::prelude::IntoRawFd;

// SPDX-License-Identifier: GPL-3.0-only
use crate::process::{ProcessEvent, ProcessHandler};
use tokio::{
	io::{AsyncBufReadExt, BufReader, Lines},
	net::UnixStream,
	sync::{
		mpsc::{self, unbounded_channel},
		oneshot,
	},
};
use tokio_util::sync::CancellationToken;

async fn receive_event(rx: &mut mpsc::UnboundedReceiver<ProcessEvent>) -> Option<()> {
	match rx.recv().await? {
		ProcessEvent::Started => {
			info!("started");
			Some(())
		}
		// cosmic-comp outputs everything to stderr because slog
		ProcessEvent::Stdout(line) | ProcessEvent::Stderr(line) => {
			info!("{}", line);
			Some(())
		}
		ProcessEvent::Ended(Some(status)) => {
			error!("exited with status {}", status);
			None
		}
		ProcessEvent::Ended(None) => {
			error!("exited");
			None
		}
	}
}

async fn receive_ipc(rx: &mut Lines<BufReader<UnixStream>>) -> Option<()> {
	let line = rx
		.next_line()
		.await
		.expect("failed to get next line of ipc")?;
	let message = serde_json::from_str::<()>(&line).expect("invalid message from cosmic-comp");
	Some(())
}

pub async fn run_compositor(token: CancellationToken, wayland_display_tx: oneshot::Sender<String>) {
	let mut wayland_display_tx = Some(wayland_display_tx);
	let (tx, mut rx) = unbounded_channel::<ProcessEvent>();
	let (session, comp) = UnixStream::pair().expect("failed to create pair of unix sockets");
	let mut session = BufReader::new(session).lines();
	let comp = {
		let std_stream = comp
			.into_std()
			.expect("failed to convert compositor unix stream to a standard unix stream");
		std_stream
			.set_nonblocking(false)
			.expect("failed to mark compositor unix stream as non-blocking");
		std_stream.into_raw_fd()
	};
	ProcessHandler::new(tx, &token).run("cosmic-comp", vec![], vec![(
		"COSMIC_SESSION_SOCK".into(),
		comp.to_string(),
	)]);
	loop {
		tokio::select! {
			exit = receive_event(&mut rx) => if exit.is_none() {
				break;
			},
			exit = receive_ipc(&mut session) => if exit.is_none() {
				break;
			}
		}
	}
}
