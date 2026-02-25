#[cfg(unix)]
use nix::sys::resource::{Resource, getrlimit};

use crate::{Result, debug};

/// This is needed for opening lots of file descriptors, which tends to
/// happen more often when using RocksDB and making lots of federation
/// connections at startup. The soft limit is usually 1024, and the hard
/// limit is usually 512000; I've personally seen it hit >2000.
///
/// * <https://www.freedesktop.org/software/systemd/man/systemd.exec.html#id-1.12.2.1.17.6>
/// * <https://github.com/systemd/systemd/commit/0abf94923b4a95a7d89bc526efc84e7ca2b71741>
#[cfg(unix)]
pub fn maximize_fd_limit() -> Result {
	use nix::sys::resource::setrlimit;

	let (soft_limit, hard_limit) = max_file_descriptors()?;
	if soft_limit < hard_limit {
		setrlimit(Resource::RLIMIT_NOFILE, hard_limit, hard_limit)?;
		assert_eq!((hard_limit, hard_limit), max_file_descriptors()?, "getrlimit != setrlimit");
		debug!(to = hard_limit, from = soft_limit, "Raised RLIMIT_NOFILE");
	}

	Ok(())
}

#[cfg(not(unix))]
pub fn maximize_fd_limit() -> Result { Ok(()) }

#[cfg(unix)]
pub fn max_file_descriptors() -> Result<(u64, u64)> {
	getrlimit(Resource::RLIMIT_NOFILE).map_err(Into::into)
}

#[cfg(not(unix))]
pub fn max_file_descriptors() -> Result<(u64, u64)> { Ok((u64::MAX, u64::MAX)) }
