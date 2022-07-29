// SPDX-License-Identifier: GPL-3.0-only
#[macro_use]
extern crate tracing;

mod comp;
mod generic;
mod process;
mod service;
mod systemd;

use async_signals::Signals;
use color_eyre::{eyre::WrapErr, Result};
use futures_util::StreamExt;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::metadata::LevelFilter;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};
use zbus::ConnectionBuilder;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
	color_eyre::install().wrap_err("failed to install color_eyre error handler")?;

	tracing_subscriber::registry()
		.with(tracing_journald::layer().wrap_err("failed to connect to journald")?)
		.with(fmt::layer())
		.with(
			EnvFilter::builder()
				.with_default_directive(LevelFilter::INFO.into())
				.from_env_lossy(),
		)
		.try_init()
		.wrap_err("failed to initialize logger")?;

	info!("Starting cosmic-session");

	let token = CancellationToken::new();
	let (socket_tx, socket_rx) = mpsc::unbounded_channel();
	let (env_tx, env_rx) = oneshot::channel();
	let compositor_handle = comp::run_compositor(token.child_token(), socket_rx, env_tx)
		.wrap_err("failed to start compositor")?;
	systemd::start_systemd_target()
		.await
		.wrap_err("failed to start systemd target")?;
	// Always stop the target when the process exits or panics.
	scopeguard::defer! {
		if let Err(error) = systemd::stop_systemd_target() {
			error!("failed to stop systemd target: {:?}", error);
		}
	}
	let env_vars = env_rx
		.await
		.expect("failed to receive environmental variables")
		.into_iter()
		.collect::<Vec<_>>();
	info!("got environmental variables: {:?}", env_vars);

	let mut sockets = Vec::with_capacity(2);

	let (env, fd) = comp::create_privileged_socket(&mut sockets, &env_vars)
		.wrap_err("failed to create panel socket")?;
	generic::run_executable(
		token.child_token(),
		info_span!(parent: None, "cosmic-panel"),
		"cosmic-panel",
		vec![],
		env,
		vec![fd],
	);
	let (env, fd) = comp::create_privileged_socket(&mut sockets, &env_vars)
		.wrap_err("failed to create applet host")?;
	generic::run_executable(
		token.child_token(),
		info_span!(parent: None, "cosmic-applet-host"),
		"cosmic-applet-host",
		vec![],
		env,
		vec![fd],
	);
	socket_tx.send(sockets).unwrap();
	generic::run_executable(
		token.child_token(),
		info_span!(parent: None, "cosmic-settings-daemon"),
		"cosmic-settings-daemon",
		vec![],
		vec![],
		vec![],
	);

	let (exit_tx, exit_rx) = oneshot::channel();
	let _ = ConnectionBuilder::session()?
		.name("com.system76.CosmicSession")?
		.serve_at("/com/system76/CosmicSession", service::SessionService {
			exit_tx: Some(exit_tx),
		})?
		.build()
		.await?;

	let mut signals = Signals::new(vec![libc::SIGTERM, libc::SIGINT]).unwrap();
	loop {
		tokio::select! {
			_ = compositor_handle => {
				info!("compositor exited");
				break;
			},
			_ = exit_rx => {
				info!("session exited by request");
				break;
			},
			signal = signals.next() => match signal {
				Some(libc::SIGTERM | libc::SIGINT) => {
					info!("received request to terminate");
					break;
				}
				Some(signal) => unreachable!("received unhandled signal {}", signal),
				None => break,
			}
		}
	}
	token.cancel();
	tokio::time::sleep(std::time::Duration::from_secs(2)).await;
	Ok(())
}
