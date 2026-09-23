use std::fmt::{Error as FmtError, Write as _};

use tuwunel_core::{
	Result,
	utils::{bytes, time},
};
use tuwunel_service::edit_purge::{OrphanedSidecarReport, OrphanedSidecarSweep, UploaderFilter};

use crate::admin_command;

#[admin_command]
pub(super) async fn delete_orphaned_long_text_sidecars(
	&self,
	older_than: String,
	uploader_prefix: Option<String>,
	uploader_regex: Option<String>,
	limit: usize,
	execute: bool,
) -> Result {
	let uploader = match (uploader_prefix, uploader_regex) {
		| (Some(prefix), _) => UploaderFilter::Prefix(prefix),
		| (None, Some(regex)) => UploaderFilter::regex(&regex)?,
		| (None, None) => UploaderFilter::Any,
	};

	let sweep = OrphanedSidecarSweep {
		older_than: time::parse_duration(&older_than)?,
		uploader,
		limit,
		execute,
	};

	let report = self
		.services
		.edit_purge
		.sweep_orphaned_long_text_sidecars(&sweep)
		.await?;

	self.write_str(&render(&sweep, &report)?).await
}

fn render(
	sweep: &OrphanedSidecarSweep,
	report: &OrphanedSidecarReport,
) -> Result<String, FmtError> {
	let size = |bytes: u64| bytes::pretty(usize::try_from(bytes).unwrap_or(usize::MAX));
	let mut out = String::new();

	writeln!(
		out,
		"Orphaned long-text sidecars older than {}{}:",
		time::pretty(sweep.older_than),
		if sweep.execute {
			""
		} else {
			" (dry run; pass --execute to delete)"
		},
	)?;
	writeln!(out, "- Uploads scanned: {}", report.uploads_scanned)?;
	writeln!(out, "- Unencrypted sidecars examined: {}", report.examined)?;
	writeln!(out, "- Encrypted sidecars skipped: {}", report.skipped_encrypted)?;
	writeln!(out, "- Retained events scanned: {}", report.events_scanned)?;
	writeln!(out, "- Kept, referenced by a retained event: {}", report.referenced)?;
	writeln!(out, "- Kept, newer than the cutoff: {}", report.too_recent)?;
	writeln!(out, "- Kept, missing from storage: {}", report.missing_storage)?;
	writeln!(out, "- Not checked, beyond the limit of {}: {}", sweep.limit, report.deferred)?;
	writeln!(out, "- Deletable: {} ({})", report.deletable, size(report.deletable_bytes))?;

	if sweep.execute {
		writeln!(out, "- Deleted: {} ({})", report.deleted, size(report.deleted_bytes))?;
		writeln!(out, "- Failed: {}", report.failed)?;
	}

	if !report.sample.is_empty() {
		writeln!(out, "\nSample of deletable sidecars:\n```")?;
		for mxc in &report.sample {
			writeln!(out, "{mxc}")?;
		}
		writeln!(out, "```")?;
	}

	if !report.failures.is_empty() {
		writeln!(out, "\nSample of failed deletions:\n```")?;
		for (mxc, failure) in &report.failures {
			writeln!(out, "{mxc}: {failure}")?;
		}
		writeln!(out, "```")?;
	}

	Ok(out)
}
