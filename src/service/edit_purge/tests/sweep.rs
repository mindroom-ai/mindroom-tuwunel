//! Orphaned long-text sidecar sweep: candidate selection, the retained-event
//! reference scan, the age cutoff and its lower bound, and the limit.

use std::time::Duration;

use regex::Regex;
use ruma::OwnedMxcUri;
use tuwunel_core::utils;

use super::{
	HarnessConfig, TestHarness, TestObject, assert_media_absent, assert_media_present,
	create_media_for_user, encrypted_long_text_sidecar_content, insert_event,
	insert_event_with_content, long_text_sidecar_content, make_harness, pdu_key,
};
use crate::edit_purge::{OrphanedSidecarReport, OrphanedSidecarSweep, UploaderFilter};

const DAY: Duration = Duration::from_hours(24);
const HOUR: Duration = Duration::from_hours(1);
const BOT: &str = "@mindroom_bot:example.com";
const ALICE: &str = "@alice:example.com";
const SIDECAR_SIZE: u64 = 4_096;

fn add_upload(
	harness: &TestHarness,
	mxc: &str,
	uploader: &str,
	content_type: Option<&str>,
	filename: Option<&str>,
	age: Option<Duration>,
) {
	create_media_for_user(harness, mxc, uploader);
	let modified_ms = age.map(|age| {
		let age_ms = u64::try_from(age.as_millis()).expect("age fits in u64");
		utils::millis_since_unix_epoch().saturating_sub(age_ms)
	});

	harness
		.media
		.objects
		.lock()
		.expect("test media lock")
		.insert(mxc.to_owned(), TestObject {
			content_type: content_type.map(ToOwned::to_owned),
			filename: filename.map(ToOwned::to_owned),
			size: SIDECAR_SIZE,
			modified_ms,
		});
}

fn add_sidecar(harness: &TestHarness, mxc: &str, uploader: &str, age: Duration) {
	add_upload(
		harness,
		mxc,
		uploader,
		Some("application/json"),
		Some("message-content.json"),
		Some(age),
	);
}

fn add_encrypted_sidecar(harness: &TestHarness, mxc: &str, uploader: &str, age: Duration) {
	add_upload(
		harness,
		mxc,
		uploader,
		Some("application/octet-stream"),
		Some("message-content.json.enc"),
		Some(age),
	);
}

fn sweep(uploader: UploaderFilter, limit: usize, execute: bool) -> OrphanedSidecarSweep {
	OrphanedSidecarSweep {
		older_than: DAY.saturating_mul(2),
		uploader,
		limit,
		execute,
	}
}

async fn run(harness: &TestHarness, sweep: &OrphanedSidecarSweep) -> OrphanedSidecarReport {
	harness
		.service
		.sweep_orphaned_long_text_sidecars(sweep)
		.await
		.expect("sweep succeeds")
}

#[tokio::test]
async fn sweep_reports_then_deletes_unreferenced_old_sidecar() {
	let harness = make_harness(HarnessConfig::default()).await;
	let orphan = "mxc://example.com/orphanedSidecar";
	add_sidecar(&harness, orphan, BOT, DAY.saturating_mul(3));

	let report = run(&harness, &sweep(UploaderFilter::Any, 100, false)).await;
	assert_eq!(report.examined, 1, "the sidecar is examined");
	assert_eq!(report.referenced, 0, "nothing references the sidecar");
	assert_eq!(report.deletable, 1, "the dry run reports the sidecar");
	assert_eq!(report.deletable_bytes, SIDECAR_SIZE, "the dry run reports its size");
	assert_eq!(report.sample, vec![OwnedMxcUri::from(orphan)], "the sample lists the sidecar");
	assert_eq!(report.deleted, 0, "a dry run deletes nothing");
	assert_media_present(&harness, orphan);

	let report = run(&harness, &sweep(UploaderFilter::Any, 100, true)).await;
	assert_eq!(report.deletable, 1, "the sidecar is still deletable");
	assert_eq!(report.deleted, 1, "the sidecar is deleted");
	assert_eq!(report.deleted_bytes, SIDECAR_SIZE, "its size is reported");
	assert_eq!(report.failed, 0, "no deletion fails");
	assert_media_absent(&harness, orphan);
}

#[tokio::test]
async fn sweep_keeps_sidecars_referenced_by_retained_events() {
	let harness = make_harness(HarnessConfig::default()).await;
	let service = &harness.service;
	let target_id = "$target_sweep_referenced:example.com";
	let url_mxc = "mxc://example.com/referencedByUrl";
	let file_mxc = "mxc://example.com/referencedByFileUrl";
	let nested_mxc = "mxc://example.com/referencedNested";
	let orphan = "mxc://example.com/unreferencedSidecar";
	for mxc in [url_mxc, file_mxc, nested_mxc, orphan] {
		add_sidecar(&harness, mxc, BOT, DAY.saturating_mul(3));
	}

	insert_event(service, 0, target_id, BOT, 100, None);
	insert_event_with_content(
		service,
		1,
		"$edit_sweep_url:example.com",
		BOT,
		1_000,
		long_text_sidecar_content(target_id, url_mxc),
	);
	insert_event_with_content(
		service,
		2,
		"$edit_sweep_file:example.com",
		BOT,
		2_000,
		encrypted_long_text_sidecar_content(target_id, file_mxc),
	);
	// Any retained reference protects a sidecar, whatever the event shape.
	insert_event_with_content(
		service,
		3,
		"$other_sweep_nested:example.com",
		ALICE,
		3_000,
		format!(r#"{{"body":"quote","org.example.links":[{{"href":"{nested_mxc}"}}]}}"#),
	);

	let report = run(&harness, &sweep(UploaderFilter::Any, 100, true)).await;
	assert_eq!(report.examined, 4, "every sidecar is examined");
	assert_eq!(report.referenced, 3, "three sidecars are referenced");
	assert_eq!(report.events_scanned, 4, "every retained event is scanned");
	assert_eq!(report.deleted, 1, "only the unreferenced sidecar is deleted");
	assert_media_present(&harness, url_mxc);
	assert_media_present(&harness, file_mxc);
	assert_media_present(&harness, nested_mxc);
	assert_media_absent(&harness, orphan);
}

#[tokio::test]
async fn sweep_keeps_sidecars_newer_than_cutoff() {
	let harness = make_harness(HarnessConfig::default()).await;
	let fresh = "mxc://example.com/freshSidecar";
	let day_old = "mxc://example.com/dayOldSidecar";
	add_sidecar(&harness, fresh, BOT, HOUR);
	add_sidecar(&harness, day_old, BOT, DAY.saturating_add(HOUR.saturating_mul(12)));

	let report = run(&harness, &sweep(UploaderFilter::Any, 100, true)).await;
	assert_eq!(report.examined, 2, "both sidecars are examined");
	assert_eq!(report.too_recent, 2, "both sidecars are newer than the cutoff");
	assert_eq!(report.deletable, 0, "neither sidecar is deletable");
	assert_eq!(report.deleted, 0, "neither sidecar is deleted");
	assert_media_present(&harness, fresh);
	assert_media_present(&harness, day_old);
}

#[tokio::test]
async fn sweep_skips_encrypted_sidecars() {
	let harness = make_harness(HarnessConfig::default()).await;
	let encrypted = "mxc://example.com/encryptedSidecar";
	add_encrypted_sidecar(&harness, encrypted, BOT, DAY.saturating_mul(3));

	let report = run(&harness, &sweep(UploaderFilter::Any, 100, true)).await;
	assert_eq!(report.skipped_encrypted, 1, "the encrypted sidecar is counted as skipped");
	assert_eq!(report.examined, 0, "the encrypted sidecar is not examined");
	assert_eq!(report.deleted, 0, "the encrypted sidecar is not deleted");
	assert_media_present(&harness, encrypted);
}

#[tokio::test]
async fn sweep_never_selects_other_media() {
	let harness = make_harness(HarnessConfig::default()).await;
	let old = Some(DAY.saturating_mul(3));
	let uploads = [
		("mxc://example.com/otherJson", Some("application/json"), Some("settings.json")),
		("mxc://example.com/unnamedJson", Some("application/json"), None),
		("mxc://example.com/plainText", Some("text/plain"), Some("message-content.json")),
		("mxc://example.com/image", Some("image/png"), Some("photo.png")),
		("mxc://example.com/untyped", None, Some("message-content.json")),
		(
			"mxc://example.com/opaqueJson",
			Some("application/octet-stream"),
			Some("message-content.json"),
		),
	];
	for (mxc, content_type, filename) in uploads {
		add_upload(&harness, mxc, BOT, content_type, filename, old);
	}

	let report = run(&harness, &sweep(UploaderFilter::Any, 100, true)).await;
	assert_eq!(report.uploads_scanned, uploads.len(), "every upload is read");
	assert_eq!(report.examined, 0, "no upload is a sidecar");
	assert_eq!(report.skipped_encrypted, 0, "no upload is an encrypted sidecar");
	assert_eq!(report.deleted, 0, "nothing is deleted");
	for (mxc, ..) in uploads {
		assert_media_present(&harness, mxc);
	}
}

#[tokio::test]
async fn sweep_selects_only_local_media_of_local_uploaders() {
	let harness = make_harness(HarnessConfig::default()).await;
	let local = "mxc://example.com/localSidecar";
	let remote_mxc = "mxc://remote.example/remoteSidecar";
	let remote_uploader = "mxc://example.com/remoteUploaderSidecar";
	add_sidecar(&harness, local, BOT, DAY.saturating_mul(3));
	add_sidecar(&harness, remote_mxc, BOT, DAY.saturating_mul(3));
	add_sidecar(&harness, remote_uploader, "@bot:remote.example", DAY.saturating_mul(3));

	let report = run(&harness, &sweep(UploaderFilter::Any, 100, true)).await;
	assert_eq!(report.examined, 1, "only local media of local uploaders is examined");
	assert_eq!(report.deleted, 1, "the local sidecar is deleted");
	assert_media_absent(&harness, local);
	assert_media_present(&harness, remote_mxc);
	assert_media_present(&harness, remote_uploader);
}

#[tokio::test]
async fn sweep_uploader_filter_restricts_selection() {
	let filters = [
		UploaderFilter::Prefix("@mindroom_".to_owned()),
		UploaderFilter::Regex(
			Regex::new(r"^@mindroom_[a-z]+:example\.com$").expect("valid regex"),
		),
	];
	for filter in filters {
		let harness = make_harness(HarnessConfig::default()).await;
		let bot_orphan = "mxc://example.com/botSidecar";
		let alice_orphan = "mxc://example.com/aliceSidecar";
		add_sidecar(&harness, bot_orphan, BOT, DAY.saturating_mul(3));
		add_sidecar(&harness, alice_orphan, ALICE, DAY.saturating_mul(3));
		add_encrypted_sidecar(&harness, "mxc://example.com/aliceEncrypted", ALICE, DAY);

		let report = run(&harness, &sweep(filter, 100, true)).await;
		assert_eq!(report.examined, 1, "only the matching uploader's sidecar is examined");
		assert_eq!(report.skipped_encrypted, 0, "other uploaders' sidecars are not counted");
		assert_eq!(report.deleted, 1, "the matching uploader's sidecar is deleted");
		assert_media_absent(&harness, bot_orphan);
		assert_media_present(&harness, alice_orphan);

		let report = run(&harness, &sweep(UploaderFilter::Any, 100, true)).await;
		assert_eq!(report.deleted, 1, "without a filter the other sidecar is deleted");
		assert_media_absent(&harness, alice_orphan);
	}
}

#[tokio::test]
async fn sweep_enforces_minimum_age() {
	let harness = make_harness(HarnessConfig {
		min_age_secs: 3_600,
		..Default::default()
	})
	.await;
	let service = &harness.service;
	let orphan = "mxc://example.com/minimumAgeSidecar";
	add_sidecar(&harness, orphan, BOT, DAY.saturating_mul(3));

	let min_age = DAY.saturating_add(HOUR);
	assert_eq!(
		service.orphaned_sidecar_min_age(),
		min_age,
		"min age adds a day to the purge age"
	);

	for older_than in [Duration::ZERO, HOUR, DAY, min_age.saturating_sub(Duration::from_secs(1))]
	{
		let refused = OrphanedSidecarSweep {
			older_than,
			..sweep(UploaderFilter::Any, 100, true)
		};
		let error = service
			.sweep_orphaned_long_text_sidecars(&refused)
			.await
			.expect_err("a cutoff below the minimum age is refused");
		assert!(
			error.to_string().contains("must be at least"),
			"the refusal names the minimum: {error}",
		);
	}
	assert_media_present(&harness, orphan);

	let zero_limit = sweep(UploaderFilter::Any, 0, true);
	assert!(
		service
			.sweep_orphaned_long_text_sidecars(&zero_limit)
			.await
			.is_err(),
		"a zero limit is refused",
	);
	assert_media_present(&harness, orphan);

	let accepted = OrphanedSidecarSweep {
		older_than: min_age,
		..sweep(UploaderFilter::Any, 100, true)
	};
	let report = service
		.sweep_orphaned_long_text_sidecars(&accepted)
		.await
		.expect("the minimum age itself is accepted");
	assert_eq!(report.deleted, 1, "the sidecar is older than the minimum age");
	assert_media_absent(&harness, orphan);
}

#[tokio::test]
async fn sweep_respects_limit() {
	let harness = make_harness(HarnessConfig::default()).await;
	let orphans = [
		"mxc://example.com/limitSidecarA",
		"mxc://example.com/limitSidecarB",
		"mxc://example.com/limitSidecarC",
	];
	for mxc in orphans {
		add_sidecar(&harness, mxc, BOT, DAY.saturating_mul(3));
	}

	let report = run(&harness, &sweep(UploaderFilter::Any, 2, false)).await;
	assert_eq!(report.deletable, 2, "the dry run stops at the limit");
	assert_eq!(report.deferred, 1, "the rest is deferred");

	let report = run(&harness, &sweep(UploaderFilter::Any, 2, true)).await;
	assert_eq!(report.deleted, 2, "at most the limit is deleted");
	assert_media_absent(&harness, orphans[0]);
	assert_media_absent(&harness, orphans[1]);
	assert_media_present(&harness, orphans[2]);

	let report = run(&harness, &sweep(UploaderFilter::Any, 2, true)).await;
	assert_eq!(report.deleted, 1, "the next sweep deletes the rest");
	assert_eq!(report.deferred, 0, "nothing is deferred");
	assert_media_absent(&harness, orphans[2]);
}

#[tokio::test]
async fn sweep_keeps_sidecars_missing_from_storage() {
	let harness = make_harness(HarnessConfig::default()).await;
	let missing = "mxc://example.com/missingSidecar";
	add_upload(
		&harness,
		missing,
		BOT,
		Some("application/json"),
		Some("message-content.json"),
		None,
	);

	let report = run(&harness, &sweep(UploaderFilter::Any, 100, true)).await;
	assert_eq!(report.missing_storage, 1, "the undated sidecar is counted");
	assert_eq!(report.deleted, 0, "an undated sidecar is not deleted");
	assert_media_present(&harness, missing);
}

#[tokio::test]
async fn sweep_fails_closed_on_undecodable_events() {
	let harness = make_harness(HarnessConfig::default()).await;
	let service = &harness.service;
	let referenced = "mxc://example.com/referencedByJsonRow";
	let orphan = "mxc://example.com/orphanBesideJsonRow";
	add_sidecar(&harness, referenced, BOT, DAY.saturating_mul(3));
	add_sidecar(&harness, orphan, BOT, DAY.saturating_mul(3));

	// A row that is JSON but not a PDU is still searched for references.
	let json_row = format!(r#"{{"content":{{"url":"{referenced}"}}}}"#);
	service
		.pduid_pdu
		.insert(&pdu_key(0), json_row.as_bytes());

	let report = run(&harness, &sweep(UploaderFilter::Any, 100, false)).await;
	assert_eq!(report.referenced, 1, "the JSON row protects its reference");
	assert_eq!(report.deletable, 1, "only the other sidecar is deletable");

	// A row that is not JSON at all hides its references, so nothing is
	// selected.
	service
		.pduid_pdu
		.insert(&pdu_key(1), b"\xFF not json");

	let error = service
		.sweep_orphaned_long_text_sidecars(&sweep(UploaderFilter::Any, 100, true))
		.await
		.expect_err("an unreadable event refuses the sweep");
	assert!(
		error.to_string().contains("could not be read"),
		"the refusal explains why: {error}",
	);
	assert_media_present(&harness, referenced);
	assert_media_present(&harness, orphan);
}

#[tokio::test]
async fn sweep_execute_refuses_read_only_database() {
	let harness = make_harness(HarnessConfig { read_only: true, ..Default::default() }).await;
	let orphan = "mxc://example.com/readOnlySidecar";
	add_sidecar(&harness, orphan, BOT, DAY.saturating_mul(3));

	let report = run(&harness, &sweep(UploaderFilter::Any, 100, false)).await;
	assert_eq!(report.deletable, 1, "a dry run still reports");

	assert!(
		harness
			.service
			.sweep_orphaned_long_text_sidecars(&sweep(UploaderFilter::Any, 100, true))
			.await
			.is_err(),
		"deletion is refused on a read-only database",
	);
	assert_media_present(&harness, orphan);
}
