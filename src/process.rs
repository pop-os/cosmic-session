// SPDX-License-Identifier: GPL-3.0-only
use color_eyre::eyre::{ContextCompat, Result, WrapErr};
use nix::fcntl;
use std::os::unix::prelude::*;

fn mark_as_not_cloexec(file: &impl AsFd) -> Result<()> {
	let raw_fd = file.as_fd().as_raw_fd();
	let fd_flags = fcntl::FdFlag::from_bits(
		fcntl::fcntl(raw_fd, fcntl::FcntlArg::F_GETFD)
			.wrap_err("failed to get GETFD value of stream")?,
	)
	.wrap_err("failed to get fd flags from file")?;
	fcntl::fcntl(
		raw_fd,
		fcntl::FcntlArg::F_SETFD(fd_flags.difference(fcntl::FdFlag::FD_CLOEXEC)),
	)
	.wrap_err("failed to set CLOEXEC on file")?;
	Ok(())
}
