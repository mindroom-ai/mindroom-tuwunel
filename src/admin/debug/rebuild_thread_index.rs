use tuwunel_core::Result;

use crate::admin_command;

#[admin_command]
pub(super) async fn rebuild_thread_index(&self) -> Result {
	self.services
		.threads
		.rebuild_thread_activity()
		.await?;

	let recount = self
		.services
		.threads
		.recount_thread_replies()
		.await?;

	let see_log = if recount.failed > 0 {
		" The server log names the roots that failed."
	} else {
		""
	};

	self.write_str(&format!(
		"Rebuilt the thread activity index and recounted thread replies.\nThread roots checked: \
		 {}, corrected: {}, failed: {}.{see_log}",
		recount.checked, recount.changed, recount.failed
	))
	.await
}
