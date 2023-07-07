// SPDX-License-Identifier: GPL-3.0-only

use color_eyre::{eyre::WrapErr, Result};

pub async fn start_systemd_target() -> Result<()> {
	let _ = std::process::Command::new("systemctl")
		.arg("start")
		.arg("--user")
		.arg("cosmic-session.target")
		.spawn()
		.wrap_err("Failed to start cosmic-session.target")?;
	Ok(())
}

pub fn stop_systemd_target() -> Result<()> {
	let _ = std::process::Command::new("systemctl")
		.arg("stop")
		.arg("--user")
		.arg("cosmic-session.target")
		.spawn()
		.wrap_err("Failed to stop cosmic-session.target")?;
	Ok(())
}
