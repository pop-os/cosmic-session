// SPDX-License-Identifier: GPL-3.0-only
use color_eyre::eyre::{Result, WrapErr};
use rustix::io::FdFlags;
use std::os::unix::prelude::*;

pub(crate) fn mark_as_not_cloexec(file: &impl AsFd) -> Result<()> {
	let flags = rustix::io::fcntl_getfd(file).wrap_err("failed to get GETFD value of stream")?;
	rustix::io::fcntl_setfd(file, flags.difference(FdFlags::CLOEXEC))
		.wrap_err("failed to unset CLOEXEC on file")
}
