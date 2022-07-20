// SPDX-License-Identifier: GPL-3.0-only

use color_eyre::{eyre::WrapErr, Result};
use systemd_client::manager::SystemdManagerProxy;

pub async fn start_systemd_target() -> Result<()> {
	let connection = zbus::Connection::session().await?;
	let manager = SystemdManagerProxy::new(&connection)
		.await
		.wrap_err("failed to connect to org.freedesktop.systemd1.Manager")?;
	manager
		.start_unit("cosmic-session.target", "replace")
		.await
		.wrap_err("failed to start cosmic-session.target")?;
	Ok(())
}
