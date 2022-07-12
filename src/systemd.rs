// SPDX-License-Identifier: GPL-3.0-only

use color_eyre::{
	eyre::{eyre, WrapErr},
	Result,
};
use tokio::process::Command;

pub async fn start_systemd_target() -> Result<()> {
	let output = Command::new("systemctl")
		.arg("--user")
		.arg("start")
		.arg("cosmic-session.target")
		.spawn()
		.wrap_err("failed to start systemd target")?
		.wait()
		.await
		.wrap_err("failed to wait for systemd target to start")?;

	if output.success() {
		Ok(())
	} else {
		Err(eyre!(
			"failed to start systemd target: code {}",
			output.code().unwrap_or(-1),
		))
	}
}
