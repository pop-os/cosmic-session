// SPDX-License-Identifier: GPL-3.0-only

use std::{
	path::Path,
	process::{Command, Stdio},
	sync::OnceLock,
};

use zbus::{
	Connection,
	zvariant::{Array, OwnedValue},
};

#[derive(Debug)]
pub struct EnvVar {
	pub key: String,
	pub value: String,
}

impl Into<EnvVar> for (&str, &str) {
	fn into(self) -> EnvVar {
		EnvVar {
			key: self.0.to_owned(),
			value: self.1.to_owned(),
		}
	}
}

#[cfg(feature = "systemd")]
use zbus_systemd::systemd1::ManagerProxy as SystemdManagerProxy;

pub async fn set_systemd_environment(key: &str, value: &str) {
	run_optional_command(
		"systemctl",
		&["--user", "set-environment", &format!("{key}={value}")],
	)
}

pub async fn start_systemd_target() {
	run_optional_command(
		"systemctl",
		&["--user", "start", "--no-block", "cosmic-session.target"],
	)
}

pub fn stop_systemd_target() {
	run_optional_command(
		"systemctl",
		&["--user", "stop", "--no-block", "cosmic-session.target"],
	)
}

/// Determine if systemd is used as the init system. This should work on all
/// linux distributions.
pub fn is_systemd_used() -> &'static bool {
	static IS_SYSTEMD_USED: OnceLock<bool> = OnceLock::new();
	IS_SYSTEMD_USED.get_or_init(|| Path::new("/run/systemd/system").exists())
}

#[cfg(feature = "systemd")]
pub async fn get_systemd_env() -> Result<Vec<EnvVar>, zbus::Error> {
	let connection = Connection::session().await?;
	let systemd_manager = SystemdManagerProxy::new(&connection).await?;
	let systemd_env = systemd_manager.environment().await?;

	let mut out: Vec<EnvVar> = Vec::new();
	for i in systemd_env {
		if let Some(b) = i.split_once("=") {
			out.push(b.into());
		}
	}
	Ok(out)
}

#[cfg(feature = "systemd")]
/// Spawn a systemd scope unit with the given name and PIDs.
pub async fn spawn_scope(mut command: String, pids: Vec<u32>) -> Result<(), zbus::Error> {
	let connection = Connection::session().await?;
	let systemd_manager = SystemdManagerProxy::new(&connection).await?;
	let pids = OwnedValue::try_from(Array::from(pids)).unwrap();
	let properties: Vec<(String, OwnedValue)> = vec![(String::from("PIDs"), pids)];
	if command.starts_with('/') {
		// use the last component of the path as the unit name
		command = command.rsplit('/').next().unwrap().to_string();
	}
	let scope_name = format!("{}.scope", command);
	systemd_manager
		.start_transient_unit(
			scope_name.to_string(),
			String::from("replace"),
			properties,
			Vec::new(),
		)
		.await?;
	Ok(())
}

/// run a command, but log errors instead of returning them or panicking
fn run_optional_command(cmd: &str, args: &[&str]) {
	match Command::new(cmd).args(args).stdin(Stdio::null()).status() {
		Ok(status) => {
			if !status.success() {
				match status.code() {
					Some(code) => warn!("{} {}: exit code {}", cmd, args.join(" "), code),
					None => warn!("{} {}: terminated by signal", cmd, args.join(" ")),
				}
			}
		}
		Err(error) => {
			warn!("unable to start {} {}: {}", cmd, args.join(" "), error);
		}
	}
}
