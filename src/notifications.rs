use color_eyre::eyre::Context;
use color_eyre::Result;
use std::os::fd::OwnedFd;
use tokio::net::UnixStream;

pub fn create_socket() -> Result<(OwnedFd, OwnedFd)> {
	// Create a new pair of unnamed Unix sockets
	let (sock_1, sock_2) = UnixStream::pair().wrap_err("failed to create socket pair")?;

	// Turn the other socket into a non-blocking fd, which we can pass to the child
	// process
	let fd_1 = {
		let std_stream = sock_1
			.into_std()
			.wrap_err("failed to convert client socket to std socket")?;
		std_stream
			.set_nonblocking(false)
			.wrap_err("failed to mark client socket as blocking")?;
		OwnedFd::from(std_stream)
	};

	let fd_2 = {
		let std_stream = sock_2
			.into_std()
			.wrap_err("failed to convert client socket to std socket")?;
		std_stream
			.set_nonblocking(false)
			.wrap_err("failed to mark client socket as blocking")?;
		OwnedFd::from(std_stream)
	};
	Ok((fd_1, fd_2))
}
