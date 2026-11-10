// SPDX-License-Identifier: GPL-3.0-only
use crate::service::SessionRequest;
use color_eyre::eyre::{Result, WrapErr};
use futures_util::AsyncRead;
use launch_pad::ProcessManager;
use launch_pad::process::Process;
use std::collections::HashMap;
use std::pin::{Pin, pin};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedReadHalf;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::compat::{Compat, TokioAsyncReadCompatExt};
use tokio_util::sync::CancellationToken;

struct IpcWrapper(Compat<OwnedReadHalf>);

impl cosmic_comp_ipc::Readable for IpcWrapper {
	fn readable(&self) -> impl Future<Output = std::io::Result<()>> {
		self.0.get_ref().readable()
	}
}

impl AsyncRead for IpcWrapper {
	fn poll_read(
		mut self: Pin<&mut Self>,
		cx: &mut std::task::Context<'_>,
		buf: &mut [u8],
	) -> std::task::Poll<std::io::Result<usize>> {
		AsyncRead::poll_read(pin!(&mut self.0), cx, buf)
	}

	fn poll_read_vectored(
		mut self: Pin<&mut Self>,
		cx: &mut std::task::Context<'_>,
		bufs: &mut [std::io::IoSliceMut<'_>],
	) -> std::task::Poll<std::io::Result<usize>> {
		AsyncRead::poll_read_vectored(pin!(&mut self.0), cx, bufs)
	}
}

async fn receive_ipc(
	env_tx: &mut Option<oneshot::Sender<HashMap<String, String>>>,
	rx: Pin<&mut IpcWrapper>,
) -> Result<()> {
	let msg = cosmic_comp_ipc::read_msg(rx)
		.await
		.wrap_err("Failed to read cosmic-comp ipc message")?;
	match msg {
		cosmic_comp_ipc::Message::SetEnv { variables } => {
			if let Some(env_tx) = env_tx.take() {
				env_tx.send(variables).unwrap();
			}
		}
	}
	Ok(())
}

pub fn run_compositor(
	process_manager: &ProcessManager,
	exec: String,
	args: Vec<String>,
	env: Vec<(String, String)>,
	_token: CancellationToken,
	env_tx: oneshot::Sender<HashMap<String, String>>,
	session_dbus_tx: mpsc::Sender<SessionRequest>,
) -> Result<JoinHandle<Result<()>>> {
	let process_manager = process_manager.clone();
	// Create a pair of unix sockets for the ipc channel to the compositor
	let (session_sock, comp_env) =
		cosmic_comp_ipc::setup_socket().wrap_err("Failed to create compositor ipc sockets")?;
	session_sock
		.set_nonblocking(true)
		.wrap_err("Failed to turn session sock nonblocking")?;
	let (session_rx, _session_tx) = UnixStream::from_std(session_sock)
		.wrap_err("Failed to convert unix stream")?
		.into_split();
	let mut session_rx = Box::pin(IpcWrapper(session_rx.compat()));
	let mut env_tx = Some(env_tx);

	Ok(tokio::spawn(async move {
		// Create a new process handler for cosmic-comp
		process_manager
			.start_process(
				Process::new()
					.with_executable(exec)
					.with_args(args)
					.with_env(env.into_iter().chain(comp_env))
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

		loop {
			if let Err(err) = receive_ipc(&mut env_tx, session_rx.as_mut()).await {
				error!("failed to receive IPC: {:?}", err);
				break;
			}
		}
		Result::<()>::Ok(())
	}))
}
