use nix::sys::resource::Usage;
#[cfg(unix)]
use nix::sys::resource::{UsageWho, getrusage};

use crate::Result;

#[cfg(unix)]
pub fn usage() -> Result<Usage> { getrusage(UsageWho::RUSAGE_SELF).map_err(Into::into) }

#[cfg(not(unix))]
pub fn usage() -> Result<Usage> { Ok(Usage::default()) }

#[cfg(any(
	target_os = "linux",
	target_os = "freebsd",
	target_os = "openbsd"
))]
pub fn thread_usage() -> Result<Usage> { getrusage(UsageWho::RUSAGE_THREAD).map_err(Into::into) }

#[cfg(not(any(
	target_os = "linux",
	target_os = "freebsd",
	target_os = "openbsd"
)))]
pub fn thread_usage() -> Result<Usage> { Ok(Usage::default()) }
