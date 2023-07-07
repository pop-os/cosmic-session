use color_eyre::eyre::Context;
use color_eyre::Result;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;

pub fn create_socket() -> Result<(OwnedFd, OwnedFd)> {
	// Create a new pair of unnamed Unix sockets
	let (sock_1, sock_2) = UnixStream::pair().wrap_err("failed to create socket pair")?;

	// Turn the sockets into non-blocking fd, which we can pass to the child
	// process
	sock_1
		.set_nonblocking(false)
		.wrap_err("failed to mark client socket as blocking")?;

	sock_2
		.set_nonblocking(false)
		.wrap_err("failed to mark client socket as blocking")?;

	Ok((OwnedFd::from(sock_1), OwnedFd::from(sock_2)))
}
