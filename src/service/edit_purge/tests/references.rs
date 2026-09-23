//! The retained-reference scan shared by the purge and the orphaned-sidecar
//! sweep also covers the retained unredacted originals of redacted events and
//! outlier events, so media a moderator can still reach is never deleted.

use std::time::Duration;

use super::{
	HarnessConfig, TestHarness, assert_event_absent, assert_event_present, assert_media_absent,
	assert_media_present, create_media_for_user, insert_event, insert_event_with_content,
	long_text_sidecar_content, make_pdu_with_content, sweep::add_sidecar,
};
use crate::edit_purge::{OrphanedSidecarSweep, UploaderFilter};

const ALICE: &str = "@alice:example.com";

/// Store `event_id` redacted in the timeline, with its unredacted original
/// (carrying `content`) retained the way a redaction saves it.
fn insert_redacted_with_original(
	harness: &TestHarness,
	key_index: u32,
	event_id: &str,
	content: String,
) {
	let service = &harness.service;
	insert_event_with_content(service, key_index, event_id, ALICE, 500, "{}".to_owned());

	let original = make_pdu_with_content(event_id, ALICE, 500, content);
	service
		.eventid_originalpdu
		.insert(event_id.as_bytes(), serde_json::to_vec(&original).expect("serialize original"));
}

fn long_text_message_content(mxc: &str) -> String {
	format!(
		r#"{{
			"body":"message-content.json",
			"msgtype":"m.file",
			"url":"{mxc}",
			"io.mindroom.long_text":{{"version":2,"encoding":"matrix_event_content_json"}}
		}}"#
	)
}

#[tokio::test]
async fn purge_keeps_sidecar_referenced_by_retained_redacted_original() {
	let harness = super::make_harness(HarnessConfig::default()).await;
	let service = &harness.service;
	let target_id = "$target_redacted_reference:example.com";
	let shared_mxc = "mxc://example.com/redactedOriginalSidecar";
	let latest_mxc = "mxc://example.com/redactedOriginalLatest";
	create_media_for_user(&harness, shared_mxc, ALICE);
	create_media_for_user(&harness, latest_mxc, ALICE);

	let target = insert_event(service, 0, target_id, ALICE, 100, None);
	let old_edit = insert_event_with_content(
		service,
		1,
		"$edit_redacted_reference_old:example.com",
		ALICE,
		1_000,
		long_text_sidecar_content(target_id, shared_mxc),
	);
	let latest_edit = insert_event_with_content(
		service,
		2,
		"$edit_redacted_reference_new:example.com",
		ALICE,
		2_000,
		long_text_sidecar_content(target_id, latest_mxc),
	);
	insert_redacted_with_original(
		&harness,
		3,
		"$redacted_reference:example.com",
		long_text_message_content(shared_mxc),
	);

	service
		.purge_cycle()
		.await
		.expect("purge cycle succeeds");

	assert_event_present(service, &target);
	assert_event_absent(service, &old_edit);
	assert_event_present(service, &latest_edit);
	assert_media_present(&harness, shared_mxc);
	assert_media_present(&harness, latest_mxc);
}

#[tokio::test]
async fn sweep_keeps_sidecars_referenced_by_retained_originals_and_outliers() {
	let harness = super::make_harness(HarnessConfig::default()).await;
	let service = &harness.service;
	let redacted_mxc = "mxc://example.com/sweepRedactedOriginal";
	let outlier_mxc = "mxc://example.com/sweepOutlier";
	let orphan = "mxc://example.com/sweepBesideOriginals";
	let age = Duration::from_hours(72);
	for mxc in [redacted_mxc, outlier_mxc, orphan] {
		add_sidecar(&harness, mxc, "@mindroom_bot:example.com", age);
	}

	insert_redacted_with_original(
		&harness,
		0,
		"$sweep_redacted:example.com",
		long_text_message_content(redacted_mxc),
	);
	let outlier = make_pdu_with_content(
		"$sweep_outlier:example.com",
		ALICE,
		600,
		long_text_message_content(outlier_mxc),
	);
	service.eventid_outlierpdu.insert(
		outlier.event_id.as_bytes(),
		serde_json::to_vec(&outlier).expect("serialize outlier"),
	);

	let sweep = OrphanedSidecarSweep {
		older_than: Duration::from_hours(48),
		uploader: UploaderFilter::Any,
		limit: 100,
		execute: true,
	};
	let report = service
		.sweep_orphaned_long_text_sidecars(&sweep)
		.await
		.expect("sweep succeeds");

	assert_eq!(report.events_scanned, 3, "timeline, original and outlier rows are scanned");
	assert_eq!(report.referenced, 2, "the original and the outlier protect their sidecars");
	assert_eq!(report.deleted, 1, "only the unreferenced sidecar is deleted");
	assert_media_present(&harness, redacted_mxc);
	assert_media_present(&harness, outlier_mxc);
	assert_media_absent(&harness, orphan);

	// An unreadable retained original hides its references, so nothing is
	// selected.
	add_sidecar(&harness, orphan, "@mindroom_bot:example.com", age);
	service
		.eventid_originalpdu
		.insert(b"$sweep_unreadable:example.com", b"\xFF not json");
	let error = service
		.sweep_orphaned_long_text_sidecars(&sweep)
		.await
		.expect_err("an unreadable original refuses the sweep");
	assert!(
		error.to_string().contains("could not be read"),
		"the refusal explains why: {error}"
	);
	assert_media_present(&harness, orphan);
}
