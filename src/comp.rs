// SPDX-License-Identifier: GPL-3.0-only
use crate::process::{ProcessEvent, ProcessHandler};
use color_eyre::eyre::{ContextCompat, Result, WrapErr};
use nix::fcntl;
use sendfd::SendWithFd;
use serde::{Deserialize, Serialize};
use std::os::unix::prelude::{AsRawFd, IntoRawFd};
use tokio::{
	io::AsyncWriteExt,
	net::UnixStream,
	sync::mpsc::{self, unbounded_channel},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "message")]
pub enum Message {
	NewPrivilegedClient { count: usize },
}

fn mark_as_not_cloexec(stream: &UnixStream) -> Result<()> {
	let raw_fd = stream.as_raw_fd();
	let fd_flags = fcntl::FdFlag::from_bits(
		fcntl::fcntl(raw_fd, fcntl::FcntlArg::F_GETFD)
			.wrap_err("failed to get GETFD value of stream")?,
	)
	.wrap_err("failed to get fd flags from file")?;
	fcntl::fcntl(
		raw_fd,
		fcntl::FcntlArg::F_SETFD(fd_flags.difference(fcntl::FdFlag::FD_CLOEXEC)),
	)
	.wrap_err("failed to set CLOEXEC on file")?;
	Ok(())
}

pub fn create_privileged_socket(
	sockets: &mut Vec<UnixStream>,
	env_vars: &[(String, String)],
) -> Result<Vec<(String, String)>> {
	let (comp_socket, client_socket) =
		UnixStream::pair().wrap_err("failed to create socket pair")?;
	sockets.push(comp_socket);
	let client_fd = {
		mark_as_not_cloexec(&client_socket).wrap_err("failed to mark client stream as CLOEXEC")?;
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

async fn send_fd(session_tx: &mut UnixStream, stream: Vec<UnixStream>) -> Result<()> {
	let fds = stream
		.into_iter()
		.map(|stream| {
			mark_as_not_cloexec(&stream).wrap_err("failed to mark stream as CLOEXEC")?;
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
	session_tx
		.send_with_fd(&[0], &fds)
		.wrap_err("failed to send fd")?;
	info!("sent {} fds", fds.len());
	Ok(())
}

pub async fn run_compositor(
	token: CancellationToken,
	mut socket_rx: mpsc::UnboundedReceiver<Vec<UnixStream>>,
) -> Result<()> {
	let (tx, mut rx) = unbounded_channel::<ProcessEvent>();
	let (mut session, comp) =
		UnixStream::pair().wrap_err("failed to create pair of unix sockets")?;
	let comp = {
		mark_as_not_cloexec(&comp).wrap_err("failed to mark compositor stream as CLOEXEC")?;
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
			Some(socket) = socket_rx.recv() => {
				send_fd(&mut session, socket)
					.await
					.wrap_err("failed to send file descriptor to compositor")?;
			}
		}
	}
	Ok(())
}
