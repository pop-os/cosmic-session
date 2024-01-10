// SPDX-License-Identifier: GPL-3.0-only

use std::process::{Command, Stdio};

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
