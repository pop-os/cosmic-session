// SPDX-License-Identifier: GPL-3.0-only
use color_eyre::eyre::{Result, WrapErr};
use launch_pad::{process::Process, ProcessManager};
use sendfd::SendWithFd;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, os::unix::prelude::*};
use tokio::{
	io::{AsyncReadExt, AsyncWriteExt},
	net::{
		unix::{OwnedReadHalf, OwnedWriteHalf},
		UnixStream,
	},
	sync::{mpsc, oneshot},
	task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use crate::{process::mark_as_not_cloexec, service::SessionRequest};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "message")]
pub enum Message {
	SetEnv { variables: HashMap<String, String> },
	NewPrivilegedClient { count: usize },
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
	// This is kind of a doozy, but this is kinda complex so it can be
	// cancellation-safe.
	match state.length {
		// We already got the length, and are currently reading the message body.
		Some(length) => {
			let index = state.bytes_read.saturating_sub(1);
			// Add the amount of bytes read to our state.
			// I don't think this is entirely cancellation safe, which worries me.
			state.bytes_read += rx
				.read_exact(&mut state.buf[index..])
				.await
				.wrap_err("failed to read IPC length")?;
			// If we've read enough bytes, parse the message.
			if state.bytes_read >= length as usize {
				parse_and_handle_ipc(state);
				// Set the state back to the default "waiting for a length" mode.
				state.length = None;
				state.bytes_read = 0;
				state.buf.clear();
			}
			Ok(())
		}
		None => {
			// Resize the state buffer enough to fit a u16./
			state.buf.resize(2, 0);
			let index = state.bytes_read.saturating_sub(1);
			// Read the remaining bytes of the length.
			state.bytes_read += rx
				.read_exact(&mut state.buf[index..])
				.await
				.wrap_err("failed to read IPC length")?;
			// If we've read two bytes, then parse a native-endian u16 from them.
			if state.bytes_read >= 2 {
				let length = u16::from_ne_bytes(
					state.buf[..2]
						.try_into()
						.wrap_err("failed to convert IPC length to u16")?,
				);
				// Set the state to "reading the message body" mode, as we now have the length.
				state.length = Some(length);
				state.bytes_read = 0;
				state.buf.resize(length as usize, 0);
			}
			Ok(())
		}
	}
}

pub fn create_privileged_socket(
	sockets: &mut Vec<UnixStream>,
	env_vars: &[(String, String)],
) -> Result<(Vec<(String, String)>, OwnedFd)> {
	// Create a new pair of unnamed Unix sockets
	let (comp_socket, client_socket) =
		UnixStream::pair().wrap_err("failed to create socket pair")?;
	// Push one socket to the list of sockets we were passed
	sockets.push(comp_socket);
	// Turn the other socket into a non-blocking fd, which we can pass to the child
	// process
	let client_fd = {
		let std_stream = client_socket
			.into_std()
			.wrap_err("failed to convert client socket to std socket")?;
		std_stream
			.set_nonblocking(true)
			.wrap_err("failed to mark client socket as non-blocking")?;
		OwnedFd::from(std_stream)
	};
	let mut env_vars = env_vars.to_vec();
	env_vars.push(("WAYLAND_SOCKET".into(), client_fd.as_raw_fd().to_string()));
	Ok((env_vars, client_fd))
}

async fn send_fd(session_tx: &mut OwnedWriteHalf, stream: Vec<UnixStream>) -> Result<()> {
	// Turn our list of Unix streams into non-blocking file descriptors.
	let fds = stream
		.into_iter()
		.map(|stream| {
			let std_stream = stream
				.into_std()
				.wrap_err("failed to convert stream to std stream")?;
			std_stream
				.set_nonblocking(false)
				.wrap_err("failed to set stream as blocking")?;
			Ok(OwnedFd::from(std_stream))
		})
		.collect::<Result<Vec<_>>>()
		.wrap_err("failed to convert streams to file descriptors")?;
	// Create a NewPrivilegedClient message, with a count of how many file
	// descriptors we are about to send.
	let json = serde_json::to_string(&Message::NewPrivilegedClient { count: fds.len() })
		.wrap_err("failed to encode json")?;
	// Send the length of our NewPrivilegedClient message.
	session_tx
		.write_all(&(json.len() as u16).to_le_bytes())
		.await
		.wrap_err("failed to write length")?;
	// Send our NewPrivilegedClient message, in JSON form.
	session_tx
		.write_all(json.as_bytes())
		.await
		.wrap_err("failed to write json")?;
	// Wait 100 us for the session to acknowledge our message.
	tokio::time::sleep(std::time::Duration::from_micros(100)).await;
	// Send our file descriptors.
	let fd: &UnixStream = session_tx.as_ref();
	info!("sending {} fds", fds.len());

	fd.send_with_fd(
		&[0],
		&fds.into_iter()
			.map(|fd| fd.into_raw_fd())
			.collect::<Vec<_>>(),
	)
	.wrap_err("failed to send fd")?;
	Ok(())
}

pub fn run_compositor(
	process_manager: &ProcessManager,
	exec: String,
	args: Vec<String>,
	_token: CancellationToken,
	mut socket_rx: mpsc::UnboundedReceiver<Vec<UnixStream>>,
	env_tx: oneshot::Sender<HashMap<String, String>>,
	session_dbus_tx: mpsc::Sender<SessionRequest>,
) -> Result<JoinHandle<Result<()>>> {
	let process_manager = process_manager.clone();
	// Create a pair of unix sockets - one for us (session),
	// one for the compositor (comp)
	let (session, comp) = UnixStream::pair().wrap_err("failed to create pair of unix sockets")?;
	let (mut session_rx, mut session_tx) = session.into_split();
	// Convert our compositor socket to a non-blocking file descriptor.
	let comp = {
		let std_stream = comp
			.into_std()
			.wrap_err("failed to convert compositor unix stream to a standard unix stream")?;
		std_stream
			.set_nonblocking(false)
			.wrap_err("failed to mark compositor unix stream as blocking")?;
		OwnedFd::from(std_stream)
	};
	mark_as_not_cloexec(&comp).expect("Failed to mark fd as not cloexec");
	Ok(tokio::spawn(async move {
		// Create a new process handler for cosmic-comp, with our compositor socket's
		// file descriptor as the `COSMIC_SESSION_SOCK` environment variable.
		process_manager
			.start_process(
				Process::new()
					.with_executable(exec)
					.with_args(args)
					.with_env([("COSMIC_SESSION_SOCK", comp.as_raw_fd().to_string())])
					.with_on_exit(move |pman, _, err_code, _will_restart| {
						let session_dbus_tx = session_dbus_tx.clone();
						async move {
							pman.stop();
							if err_code == Some(0) {
								info!("cosmic-comp exited successfully");
								session_dbus_tx.send(SessionRequest::Exit).await.unwrap();
							} else if let Some(err_code) = err_code {
								error!("cosmic-comp exited with error code {}", err_code);
								session_dbus_tx.send(SessionRequest::Restart).await.unwrap();
							} else {
								warn!("cosmic-comp exited by signal");
								session_dbus_tx.send(SessionRequest::Restart).await.unwrap();
							}
						}
					}),
			)
			.await
			.expect("failed to launch compositor");
		// Create a new state object for IPC purposes.
		let mut ipc_state = IpcState {
			env_tx: Some(env_tx),
			..IpcState::default()
		};
		loop {
			tokio::select! {
				/*
				exit = receive_event(&mut rx) => if exit.is_none() {
					break;
				},
				*/
				// Receive IPC messages from the process,
				// exiting the loop if IPC errors.
				result = receive_ipc(&mut ipc_state, &mut session_rx) => if let Err(err) = result {
					error!("failed to receive IPC: {:?}", err);
					break;
				},
				// Send any file descriptors we need to the compositor.
				Some(socket) = socket_rx.recv() => {
					send_fd(&mut session_tx, socket)
						.await
						.wrap_err("failed to send file descriptor to compositor")?;
				}
			}
		}
		Result::<()>::Ok(())
	}))
}
