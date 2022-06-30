// SPDX-License-Identifier: GPL-3.0-only
use crate::process::{ProcessEvent, ProcessHandler};
use color_eyre::eyre::{ContextCompat, Result, WrapErr};
use sendfd::SendWithFd;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, os::unix::prelude::IntoRawFd};
use tokio::{
	io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
	net::{
		unix::{ReadHalf, WriteHalf},
		UnixStream,
	},
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
	NewPrivilegedClient,
}

pub fn create_privileged_socket(
	sockets: &mut Vec<UnixStream>,
	env_vars: &[(String, String)],
) -> Result<Vec<(String, String)>> {
	let (comp_socket, client_socket) =
		UnixStream::pair().wrap_err("failed to create socket pair")?;
	sockets.push(comp_socket);
	let client_fd = {
		let std_stream = client_socket
			.into_std()
			.wrap_err("failed to convert client socket to std socket")?;
		std_stream
			.set_nonblocking(false)
			.wrap_err("failed to mark client socket as blocking")?;
		std_stream.into_raw_fd()
	};
	let mut env_vars = env_vars.to_vec();
	env_vars.push(("WAYLAND_SOCKET".into(), client_fd.to_string()));
	Ok(env_vars)
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
) -> Result<Option<()>> {
	let line = match rx
		.next_line()
		.await
		.wrap_err("failed to get next line of ipc")?
	{
		Some(line) => line,
		None => return Ok(None),
	};
	match serde_json::from_str::<Message>(&line).wrap_err("invalid message from cosmic-comp")? {
		Message::SetEnv { variables } => {
			if let Some(env_tx) = env_tx.take() {
				env_tx
					.send(variables.into_iter().collect())
					.expect("failed to send environmental variables");
			}
		}
		Message::NewPrivilegedClient => {
			unreachable!("compositor should not send NewPrivilegedClient")
		}
	}
	Ok(Some(()))
}

async fn send_fd(session_tx: &mut WriteHalf<'_>, stream: Vec<UnixStream>) -> Result<()> {
	let fds = stream
		.into_iter()
		.map(|stream| {
			let std_stream = stream
				.into_std()
				.wrap_err("failed to convert stream to std stream")?;
			std_stream
				.set_nonblocking(false)
				.wrap_err("failed to set stream as blocking")?;
			Ok(std_stream.into_raw_fd())
		})
		.collect::<Result<Vec<_>>>()
		.wrap_err("failed to convert streams to file descriptors")?;
	let json =
		serde_json::to_string(&Message::NewPrivilegedClient).wrap_err("failed to encode json")?;
	session_tx
		.write_all(json.as_bytes())
		.await
		.wrap_err("failed to write json")?;
	session_tx
		.write_all(b"\n")
		.await
		.wrap_err("failed to write newline")?;
	let tx: &UnixStream = session_tx.as_ref();
	tx.send_with_fd(&[], &fds).wrap_err("failed to send fd")?;
	Ok(())
}

pub async fn run_compositor(
	token: CancellationToken,
	mut socket_rx: mpsc::UnboundedReceiver<Vec<UnixStream>>,
	env_tx: oneshot::Sender<Vec<(String, String)>>,
) -> Result<()> {
	let mut env_tx = Some(env_tx);
	let (tx, mut rx) = unbounded_channel::<ProcessEvent>();
	let (mut session, comp) =
		UnixStream::pair().wrap_err("failed to create pair of unix sockets")?;
	let (session_rx, mut session_tx) = session.split();
	let mut session = BufReader::new(session_rx).lines();
	let comp = {
		let std_stream = comp
			.into_std()
			.wrap_err("failed to convert compositor unix stream to a standard unix stream")?;
		std_stream
			.set_nonblocking(false)
			.wrap_err("failed to mark compositor unix stream as blocking")?;
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
			exit = receive_ipc(&mut session, &mut env_tx) => if exit.wrap_err("failed to receive ipc message")?.is_none() {
				break;
			},
			Some(socket) = socket_rx.recv() => {
				send_fd(&mut session_tx, socket)
					.await
					.wrap_err("failed to send file descriptor to compositor")?;
			}
		}
	}
	Ok(())
}
