// SPDX-License-Identifier: GPL-3.0-only
use crate::process::{ProcessEvent, ProcessHandler};
use color_eyre::eyre::{Result, WrapErr};
use nix::unistd;
use sendfd::SendWithFd;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, os::unix::prelude::*};
use tokio::{
	io::{AsyncReadExt, AsyncWriteExt},
	net::{
		unix::{OwnedReadHalf, OwnedWriteHalf},
		UnixStream,
	},
	sync::{
		mpsc::{self, unbounded_channel},
		oneshot,
	},
	task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "message")]
pub enum Message {
	SetEnv { variables: HashMap<String, String> },
	NewPrivilegedClient { count: usize },
}

pub fn create_privileged_socket(
	sockets: &mut Vec<UnixStream>,
	env_vars: &[(String, String)],
) -> Result<(Vec<(String, String)>, RawFd)> {
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
	Ok((env_vars, client_fd))
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

// Cancellation safe!
#[derive(Default)]
struct IpcState {
	env_tx: Option<oneshot::Sender<HashMap<String, String>>>,
	length: Option<u16>,
	bytes_read: usize,
	buf: Vec<u8>,
}

fn parse_and_handle_ipc(state: &mut IpcState) {
	match serde_json::from_slice::<Message>(&state.buf) {
		Ok(Message::SetEnv { variables }) => {
			if let Some(env_tx) = state.env_tx.take() {
				env_tx.send(variables).unwrap();
			}
		}
		Ok(Message::NewPrivilegedClient { .. }) => {
			unreachable!("NewPrivilegedClient should not be sent TO the session!");
		}
		Err(_) => {
			warn!(
				"Unknown session socket message, are you using incompatible cosmic-session and \
				 cosmic-comp versions?"
			)
		}
	}
}

async fn receive_ipc(state: &mut IpcState, rx: &mut OwnedReadHalf) -> Result<()> {
	match state.length {
		Some(length) => {
			let index = state.bytes_read.saturating_sub(1);
			state.bytes_read += rx
				.read_exact(&mut state.buf[index..])
				.await
				.wrap_err("failed to read IPC length")?;
			if state.bytes_read >= length as usize {
				parse_and_handle_ipc(state);
				state.length = None;
				state.bytes_read = 0;
				state.buf.clear();
			}
			Ok(())
		}
		None => {
			state.buf.resize(2, 0);
			let index = state.bytes_read.saturating_sub(1);
			state.bytes_read += rx
				.read_exact(&mut state.buf[index..])
				.await
				.wrap_err("failed to read IPC length")?;
			if state.bytes_read >= 2 {
				let length = u16::from_ne_bytes(
					state.buf[..2]
						.try_into()
						.wrap_err("failed to convert IPC length to u16")?,
				);
				state.length = Some(length);
				state.bytes_read = 0;
				state.buf.resize(length as usize, 0);
			}
			Ok(())
		}
	}
}

async fn send_fd(session_tx: &mut OwnedWriteHalf, stream: Vec<UnixStream>) -> Result<()> {
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
	let json = serde_json::to_string(&Message::NewPrivilegedClient { count: fds.len() })
		.wrap_err("failed to encode json")?;
	session_tx
		.write_all(&(json.len() as u16).to_le_bytes())
		.await
		.wrap_err("failed to write length")?;
	session_tx
		.write_all(json.as_bytes())
		.await
		.wrap_err("failed to write json")?;
	tokio::time::sleep(std::time::Duration::from_micros(100)).await;
	let fd: &UnixStream = session_tx.as_ref();
	fd.send_with_fd(&[0], &fds).wrap_err("failed to send fd")?;
	for fd in &fds {
		let _ = unistd::close(*fd);
	}
	info!("sent {} fds", fds.len());
	Ok(())
}

pub fn run_compositor(
	token: CancellationToken,
	mut socket_rx: mpsc::UnboundedReceiver<Vec<UnixStream>>,
	env_tx: oneshot::Sender<HashMap<String, String>>,
) -> Result<JoinHandle<Result<()>>> {
	let (tx, mut rx) = unbounded_channel::<ProcessEvent>();
	let (session, comp) = UnixStream::pair().wrap_err("failed to create pair of unix sockets")?;
	let (mut session_rx, mut session_tx) = session.into_split();
	let comp = {
		let std_stream = comp
			.into_std()
			.wrap_err("failed to convert compositor unix stream to a standard unix stream")?;
		std_stream
			.set_nonblocking(false)
			.wrap_err("failed to mark compositor unix stream as blocking")?;
		std_stream.into_raw_fd()
	};
	let span = info_span!(parent: None, "cosmic-comp");
	let _span = span.clone();
	Ok(tokio::spawn(
		async move {
			ProcessHandler::new(tx, &token).run(
				"cosmic-comp",
				vec![],
				vec![("COSMIC_SESSION_SOCK".into(), comp.to_string())],
				vec![comp],
				&span,
			);
			let mut ipc_state = IpcState {
				env_tx: Some(env_tx),
				..IpcState::default()
			};
			loop {
				tokio::select! {
					exit = receive_event(&mut rx) => if exit.is_none() {
						break;
					},
					result = receive_ipc(&mut ipc_state, &mut session_rx) => if let Err(err) = result {
						error!("failed to receive IPC: {:?}", err);
					},
					Some(socket) = socket_rx.recv() => {
						send_fd(&mut session_tx, socket)
							.await
							.wrap_err("failed to send file descriptor to compositor")?;
					}
				}
			}
			Result::<()>::Ok(())
		}
		.instrument(_span),
	))
}
