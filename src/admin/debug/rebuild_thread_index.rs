use tuwunel_core::Result;

use crate::admin_command;

#[admin_command]
pub(super) async fn rebuild_thread_index(&self) -> Result {
	self.services
		.threads
		.rebuild_thread_activity()
		.await?;

	let changed = self
		.services
		.threads
		.recount_thread_replies()
		.await?;

	self.write_str(&format!(
		"Rebuilt the thread activity index and corrected the reply count of {changed} thread \
		 roots."
	))
	.await
}
