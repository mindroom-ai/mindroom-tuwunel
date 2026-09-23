//! One-shot sweep of MindRoom long-text sidecar media that no retained event
//! references.
//!
//! The purge deletes a purged edit's sidecar together with the edit. Sidecars
//! whose referencing events are already gone (for example, purged by an older
//! release that did not recognize every sidecar shape) can no longer be found
//! from any event, so this sweep starts from the media side instead: it lists
//! the local uploads that look like unencrypted sidecars and deletes the ones
//! that a full scan of retained events does not reference.

use std::{collections::HashSet, time::Duration};

use futures::StreamExt;
use regex::Regex;
use ruma::{Mxc, OwnedMxcUri, OwnedUserId, ServerName, UserId};
use tokio::task::yield_now;
use tuwunel_core::{
	Err, Result, debug, info,
	utils::{self, time::pretty},
	warn,
};

use super::{SCAN_YIELD_INTERVAL, Service};

/// Content type and upload filename of an unencrypted long-text sidecar.
const SIDECAR_CONTENT_TYPE: &str = "application/json";
const SIDECAR_FILENAME: &str = "message-content.json";

/// Content type and upload filename of an encrypted-room long-text sidecar.
const ENCRYPTED_SIDECAR_CONTENT_TYPE: &str = "application/octet-stream";
const ENCRYPTED_SIDECAR_FILENAME: &str = "message-content.json.enc";

/// Added to `mindroom_edit_purge_min_age_secs` to give the shortest
/// `older_than` a sweep accepts.
pub const ORPHANED_SIDECAR_AGE_MARGIN: Duration = Duration::from_hours(24);

/// Most MXCs a report lists as a sample.
const REPORT_SAMPLE_LIMIT: usize = 20;

/// Which uploaders' media a sweep may select.
#[derive(Debug)]
pub enum UploaderFilter {
	/// Any local uploader.
	Any,
	/// Local uploaders whose full user ID starts with this prefix.
	Prefix(String),
	/// Local uploaders whose full user ID matches this expression.
	Regex(Regex),
}

impl UploaderFilter {
	fn matches(&self, user: &UserId) -> bool {
		match self {
			| Self::Any => true,
			| Self::Prefix(prefix) => user.as_str().starts_with(prefix.as_str()),
			| Self::Regex(regex) => regex.is_match(user.as_str()),
		}
	}
}

/// Parameters of one orphaned-sidecar sweep.
#[derive(Debug)]
pub struct OrphanedSidecarSweep {
	/// Only media whose stored object was last modified longer ago than this.
	/// Must be at least [`Service::orphaned_sidecar_min_age`].
	pub older_than: Duration,
	pub uploader: UploaderFilter,
	/// Most sidecars selected for deletion by one sweep.
	pub limit: usize,
	/// Delete the selected sidecars; otherwise only report them.
	pub execute: bool,
}

/// What an orphaned-sidecar sweep found, and deleted when executed.
#[derive(Debug, Default)]
pub struct OrphanedSidecarReport {
	/// Entries read from the uploader index.
	pub uploads_scanned: usize,
	/// Unencrypted local sidecar uploads matching the uploader filter.
	pub examined: usize,
	/// Encrypted-room sidecar uploads matching the uploader filter. Encrypted
	/// event content is opaque to the server, so these are never selected.
	pub skipped_encrypted: usize,
	/// Stored events scanned for references.
	pub events_scanned: usize,
	/// Examined sidecars that a retained event references.
	pub referenced: usize,
	/// Unreferenced sidecars modified more recently than the cutoff.
	pub too_recent: usize,
	/// Unreferenced sidecars with no stored object to date them.
	pub missing_storage: usize,
	/// Unreferenced sidecars left unchecked because `limit` was reached.
	pub deferred: usize,
	/// Unreferenced sidecars older than the cutoff.
	pub deletable: usize,
	pub deletable_bytes: u64,
	/// The first deletable MXCs, at most [`REPORT_SAMPLE_LIMIT`].
	pub sample: Vec<OwnedMxcUri>,
	pub deleted: usize,
	pub deleted_bytes: u64,
	pub failed: usize,
	/// The first failures with their reason, at most [`REPORT_SAMPLE_LIMIT`].
	pub failures: Vec<(OwnedMxcUri, String)>,
}

/// Stored content type and upload filename of an upload.
pub(super) struct UploadType {
	pub(super) content_type: Option<String>,
	pub(super) filename: Option<String>,
}

/// Byte length and modification time of an upload's stored object.
pub(super) struct StoredObject {
	pub(super) size: u64,
	pub(super) modified_ms: u64,
}

enum SidecarKind {
	Plain,
	Encrypted,
}

/// Recognizes the upload shape the MindRoom runtime gives long-text sidecars;
/// every other upload, including other JSON media, is not a sidecar.
fn sidecar_kind(upload: &UploadType) -> Option<SidecarKind> {
	match (upload.content_type.as_deref(), upload.filename.as_deref()) {
		| (Some(SIDECAR_CONTENT_TYPE), Some(SIDECAR_FILENAME)) => Some(SidecarKind::Plain),
		| (Some(ENCRYPTED_SIDECAR_CONTENT_TYPE), Some(ENCRYPTED_SIDECAR_FILENAME)) =>
			Some(SidecarKind::Encrypted),
		| _ => None,
	}
}

impl Service {
	/// Shortest age a sweep accepts: `mindroom_edit_purge_min_age_secs` plus
	/// [`ORPHANED_SIDECAR_AGE_MARGIN`]. Sidecars of edits the purge has not
	/// reached yet, and fresh uploads whose event has not been sent, are
	/// younger than this.
	#[must_use]
	pub fn orphaned_sidecar_min_age(&self) -> Duration {
		Duration::from_secs(
			self.services
				.server
				.config
				.mindroom_edit_purge_min_age_secs,
		)
		.saturating_add(ORPHANED_SIDECAR_AGE_MARGIN)
	}

	/// Find, and when `sweep.execute` is set delete, local unencrypted
	/// long-text sidecars that no retained event references.
	///
	/// Candidates are local uploads by a local user matching `sweep.uploader`
	/// whose stored type and filename are those of an unencrypted sidecar.
	/// One full scan of retained events removes every referenced candidate.
	/// Each remaining one is selected when its stored object is older than
	/// `sweep.older_than`, up to `sweep.limit`, and deleted only if the
	/// uploader still owns it.
	///
	/// Encrypted-room sidecars are never selected: the server cannot read the
	/// encrypted events that reference them. If any stored event cannot be
	/// read, nothing is selected.
	#[tracing::instrument(skip_all, level = "debug")]
	pub async fn sweep_orphaned_long_text_sidecars(
		&self,
		sweep: &OrphanedSidecarSweep,
	) -> Result<OrphanedSidecarReport> {
		let min_age = self.orphaned_sidecar_min_age();
		if sweep.older_than < min_age {
			return Err!(
				"The age cutoff must be at least {} (mindroom_edit_purge_min_age_secs plus {}).",
				pretty(min_age),
				pretty(ORPHANED_SIDECAR_AGE_MARGIN),
			);
		}

		if sweep.limit == 0 {
			return Err!("The limit must be at least 1.");
		}

		if sweep.execute && self.services.db.is_read_only() {
			return Err!("The database is read-only; nothing can be deleted.");
		}

		let older_than_ms = u64::try_from(sweep.older_than.as_millis()).unwrap_or(u64::MAX);
		let cutoff_ms = utils::millis_since_unix_epoch().saturating_sub(older_than_ms);

		let mut report = OrphanedSidecarReport::default();
		let candidates = self
			.long_text_sidecar_uploads(&sweep.uploader, &mut report)
			.await;

		let candidate_mxcs: HashSet<OwnedMxcUri> = candidates
			.iter()
			.map(|(mxc, _)| mxc.clone())
			.collect();

		let scan = self
			.referenced_mxcs(&candidate_mxcs, &HashSet::new())
			.await;

		drop(candidate_mxcs);
		report.events_scanned = scan.scanned;
		if scan.unreadable > 0 {
			return Err!(
				"{} stored events could not be read, so no sidecar can be proven unreferenced.",
				scan.unreadable,
			);
		}

		report.referenced = scan.protected.len();
		let mut deletable = Vec::new();
		for (checked, (mxc, uploader)) in candidates.into_iter().enumerate() {
			if checked.is_multiple_of(SCAN_YIELD_INTERVAL) {
				yield_now().await;
			}

			if scan.protected.contains(&mxc) {
				continue;
			}

			if deletable.len() >= sweep.limit {
				report.deferred = report.deferred.saturating_add(1);
				continue;
			}

			let Ok(parts) = mxc.parts() else {
				continue;
			};

			match self.services.media.stored_object(&parts).await {
				| None => report.missing_storage = report.missing_storage.saturating_add(1),
				| Some(object) if object.modified_ms >= cutoff_ms =>
					report.too_recent = report.too_recent.saturating_add(1),
				| Some(object) => {
					report.deletable = report.deletable.saturating_add(1);
					report.deletable_bytes = report.deletable_bytes.saturating_add(object.size);
					if report.sample.len() < REPORT_SAMPLE_LIMIT {
						report.sample.push(mxc.clone());
					}

					deletable.push((mxc, uploader, object.size));
				},
			}
		}

		if sweep.execute {
			for (mxc, uploader, size) in deletable {
				self.delete_orphaned_sidecar(&mxc, &uploader, size, &mut report)
					.await;

				yield_now().await;
			}

			info!(
				deleted = report.deleted,
				bytes = report.deleted_bytes,
				failed = report.failed,
				"Deleted orphaned MindRoom long-text sidecar media"
			);
		}

		Ok(report)
	}

	/// Candidate sidecars in uploader-index order: local unencrypted sidecar
	/// uploads by a local user matching `uploader`. Reads only the database.
	async fn long_text_sidecar_uploads(
		&self,
		uploader: &UploaderFilter,
		report: &mut OrphanedSidecarReport,
	) -> Vec<(OwnedMxcUri, OwnedUserId)> {
		let server_name: &ServerName = self.services.server.name.as_ref();
		let mut seen = HashSet::new();
		let mut candidates = Vec::new();
		let mut uploads = self.services.media.uploads();

		while let Some((mxc, user)) = uploads.next().await {
			report.uploads_scanned = report.uploads_scanned.saturating_add(1);
			if report
				.uploads_scanned
				.is_multiple_of(SCAN_YIELD_INTERVAL)
			{
				yield_now().await;
			}

			if user.server_name() != server_name || !uploader.matches(&user) {
				continue;
			}

			let Ok(parts) = mxc.parts() else {
				continue;
			};

			if parts.server_name != server_name {
				continue;
			}

			let kind = self
				.services
				.media
				.upload_type(&parts)
				.await
				.as_ref()
				.and_then(sidecar_kind);

			match kind {
				| Some(SidecarKind::Plain) =>
					if seen.insert(mxc.clone()) {
						candidates.push((mxc, user));
					},
				| Some(SidecarKind::Encrypted) => {
					report.skipped_encrypted = report.skipped_encrypted.saturating_add(1);
				},
				| None => {},
			}
		}

		report.examined = candidates.len();
		candidates
	}

	async fn delete_orphaned_sidecar(
		&self,
		mxc: &OwnedMxcUri,
		uploader: &UserId,
		size: u64,
		report: &mut OrphanedSidecarReport,
	) {
		let failure = match mxc.parts() {
			| Err(e) => e.to_string(),
			| Ok(parts) => match self.delete_owned(&parts, uploader).await {
				| Ok(()) => {
					debug!(%mxc, %uploader, "Deleted orphaned MindRoom long-text sidecar media");
					report.deleted = report.deleted.saturating_add(1);
					report.deleted_bytes = report.deleted_bytes.saturating_add(size);
					return;
				},
				| Err(e) => e,
			},
		};

		warn!(%mxc, %uploader, %failure, "Failed to delete orphaned MindRoom long-text sidecar");
		report.failed = report.failed.saturating_add(1);
		if report.failures.len() < REPORT_SAMPLE_LIMIT {
			report.failures.push((mxc.clone(), failure));
		}
	}

	async fn delete_owned(&self, mxc: &Mxc<'_>, uploader: &UserId) -> Result<(), String> {
		match self
			.services
			.media
			.delete_owned_by(mxc, uploader)
			.await
		{
			| Ok(true) => Ok(()),
			| Ok(false) => Err("ownership check failed".to_owned()),
			| Err(e) => Err(e.to_string()),
		}
	}
}
