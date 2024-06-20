// SPDX-License-Identifier: GPL-3.0-only

use std::process::{Command, Stdio};
use std::sync::OnceLock;

use zbus::zvariant::{Array, OwnedValue};
use zbus::{proxy, zvariant::Value, Connection};

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

///Determine if systemd is used as the init system. This should work on all linux distributions.
pub fn is_systemd_used() -> &'static bool {
	static IS_SYSTEMD_USED: OnceLock<bool> = OnceLock::new();
	IS_SYSTEMD_USED.get_or_init(
		|| match Command::new("readlink").args(&["/sbin/init"]).output() {
			Ok(output) => {
				let init = String::from_utf8_lossy(&output.stdout);
				init.trim().ends_with("/lib/systemd/systemd")
			}
			Err(error) => {
				warn!("unable to check if systemd is used: {}", error);
				false
			}
		},
	)
}

#[proxy(
	name = "org.freedesktop.systemd1.Manager",
	default_service = "org.freedesktop.systemd1",
	default_path = "/org/freedesktop/systemd1"
)]
trait SystemdManager<'a> {
	fn start_transient_unit<'a>(
		&self,
		name: &str,
		mode: &str,
		properties: Vec<(String, Value<'a>)>,
		//This is based on the systemd-zbus implementation, however according to the spec this should be empty
		//see: https://www.freedesktop.org/software/systemd/man/latest/org.freedesktop.systemd1.html#:~:text=aux%20is%20currently%20unused
		aux: Vec<(String, Vec<(String, Value<'a>)>)>,
	) -> zbus::Result<()>;
}

///Spawn a systemd scope unit with the given name and PIDs.
pub async fn spawn_scope(scope_name: &str, pids: Vec<u32>) -> zbus::Result<()> {
	let connection = Connection::session().await?;
	let systemd_manager = SystemdManagerProxy::new(&connection).await?;

	let properties = vec![(String::from("PIDs"), Value::Array(Array::from(pids)))];
	systemd_manager
		.start_transient_unit(scope_name, "fail", properties, Vec::new())
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
