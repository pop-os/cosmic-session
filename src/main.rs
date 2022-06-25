// SPDX-License-Identifier: GPL-3.0-only
#[macro_use]
extern crate tracing;

mod comp;
mod panel;
mod process;

use async_signals::Signals;
use color_eyre::{eyre::WrapErr, Result};
use futures_util::StreamExt;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::metadata::LevelFilter;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
	color_eyre::install().wrap_err("failed to install color_eyre error handler")?;

	tracing_subscriber::registry()
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
	let (wayland_display_tx, wayland_display_rx) = oneshot::channel();
	tokio::spawn(comp::run_compositor(
		token.child_token(),
		wayland_display_tx,
	));
	let wayland_display = wayland_display_rx
		.await
		.expect("failed to get WAYLAND_DISPLAY");
	info!("got WAYLAND_DISPLAY: {}", wayland_display);

	tokio::spawn(panel::run_panel(
		token.child_token(),
		"testing-panel",
		wayland_display.clone(),
	));
	tokio::spawn(panel::run_panel(
		token.child_token(),
		"testing-dock",
		wayland_display.clone(),
	));

	let mut signals = Signals::new(vec![libc::SIGTERM, libc::SIGINT]).unwrap();
	while let Some(signal) = signals.next().await {
		match signal {
			libc::SIGTERM | libc::SIGINT => {
				info!("received request to terminate");
				token.cancel();
				tokio::time::sleep(std::time::Duration::from_secs(2)).await;
				break;
			}
			_ => unreachable!("received unhandled signal {}", signal),
		}
	}

	Ok(())
}
