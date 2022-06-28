// SPDX-License-Identifier: GPL-3.0-only
use crate::process::{ProcessEvent, ProcessHandler};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, os::unix::prelude::IntoRawFd};
use tokio::{
	io::{AsyncBufReadExt, BufReader, Lines},
	net::{unix::ReadHalf, UnixStream},
	sync::{
		mpsc::{self, unbounded_channel},
		oneshot,
	},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "message")]
pub enum Message {
	SetEnv { variables: HashMap<String, String> },
	NewPrivilegedClient { fd: u32 },
}

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

async fn receive_ipc(
	rx: &mut Lines<BufReader<ReadHalf<'_>>>,
	env_tx: &mut Option<oneshot::Sender<Vec<(String, String)>>>,
) -> Option<()> {
	let line = rx
		.next_line()
		.await
		.expect("failed to get next line of ipc")?;
	match serde_json::from_str::<Message>(&line).expect("invalid message from cosmic-comp") {
		Message::SetEnv { variables } => {
			if let Some(env_tx) = env_tx.take() {
				env_tx
					.send(variables.into_iter().collect())
					.expect("failed to send environmental variables");
			}
		}
		Message::NewPrivilegedClient { fd } => {
			unreachable!("compositor should not send NewPrivilegedClient")
		}
	}
	Some(())
}

pub async fn run_compositor(
	token: CancellationToken,
	env_tx: oneshot::Sender<Vec<(String, String)>>,
) {
	let mut env_tx = Some(env_tx);
	let (tx, mut rx) = unbounded_channel::<ProcessEvent>();
	let (mut session, comp) = UnixStream::pair().expect("failed to create pair of unix sockets");
	let (session_rx, session_tx) = session.split();
	let mut session = BufReader::new(session_rx).lines();
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
			exit = receive_ipc(&mut session, &mut env_tx) => if exit.is_none() {
				break;
			}
		}
	}
}
