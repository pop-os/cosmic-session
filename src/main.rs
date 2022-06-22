// SPDX-License-Identifier: GPL-3.0-only
#[macro_use]
extern crate tracing;

mod process;

use color_eyre::{eyre::WrapErr, Result};
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
		.wrap_err("failed to ianitialize logger")?;

	info!("Starting cosmic-session");

	Ok(())
}
