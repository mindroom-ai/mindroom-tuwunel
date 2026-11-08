//! `/notifications` marks an entry read only by a read that covers it, as the
//! sync counts do: a thread reply by a read of its thread, a main-timeline
//! entry by a read of the main timeline through it.

#![cfg(test)]

mod support;

use std::sync::Arc;

use axum::{Router, body::Body};
use serde_json::{Value, json};
use tower::ServiceExt;
use tuwunel_api::router::state::Guard;
use tuwunel_core::{
	Result,
	http::{Method, Request, StatusCode, header},
	ruma::{OwnedEventId, OwnedRoomId, RoomId, user_id},
};
use tuwunel_service::Services;

use self::support::Harness;

const READER_TOKEN: &str = "notifications-thread-reader-token-0123456789abcdef";
const HELPER_TOKEN: &str = "notifications-thread-helper-token-0123456789abcdef";
/// The tests share one process, which takes only one global tracing subscriber.
const QUIET: &str = "log_global_default=false";

/// MSC3771: a `"main"` receipt, or the reader's own main-timeline message,
/// reads only the main timeline, so the room's thread replies stay unread in
/// `/notifications` as they do in the sync counts. An unthreaded receipt,
/// public or private, reads every thread through its event, and a later reply
/// in that thread leaves the earlier replies read. A receipt in the thread
/// reads its replies.
#[test]
fn main_read_leaves_thread_replies_unread() -> Result {
	let harness = Harness::new("mindroom_notifications_thread_read", [QUIET.to_owned()])?;

	harness.with_services(async |services| {
		let (router, _guard) = serve(services).await?;

		let room = create_room(&router).await;
		let path = format!("/rooms/{room}/join");
		request(&router, Method::POST, &path, HELPER_TOKEN, json!({})).await;

		let root = send(&router, &room, HELPER_TOKEN, "root", text("Question")).await;
		let reply = send(&router, &room, HELPER_TOKEN, "reply", thread_reply(&root)).await;
		let main = send(&router, &room, HELPER_TOKEN, "main", text("Main timeline")).await;

		receipt(&router, &room, "m.read", &main, json!({"thread_id": "main"})).await;
		let flags = read_flags(&router).await;
		assert_eq!(flag(&flags, &root), Some(true), "main receipt: {flags:?}");
		assert_eq!(flag(&flags, &main), Some(true), "main receipt: {flags:?}");
		assert_eq!(flag(&flags, &reply), Some(false), "main receipt: {flags:?}");

		send(&router, &room, READER_TOKEN, "own", text("Own message")).await;
		let flags = read_flags(&router).await;
		assert_eq!(flag(&flags, &reply), Some(false), "own main message: {flags:?}");

		receipt(&router, &room, "m.read", &main, json!({})).await;
		let flags = read_flags(&router).await;
		assert_eq!(flag(&flags, &reply), Some(true), "unthreaded receipt: {flags:?}");

		let again = send(&router, &room, HELPER_TOKEN, "again", thread_reply(&root)).await;
		let later = send(&router, &room, HELPER_TOKEN, "later", text("Later")).await;
		receipt(&router, &room, "m.read", &later, json!({"thread_id": "main"})).await;
		let flags = read_flags(&router).await;
		assert_eq!(flag(&flags, &reply), Some(true), "earlier reply: {flags:?}");
		assert_eq!(flag(&flags, &later), Some(true), "later main: {flags:?}");
		assert_eq!(flag(&flags, &again), Some(false), "later reply: {flags:?}");

		receipt(&router, &room, "m.read", &again, json!({"thread_id": root})).await;
		let flags = read_flags(&router).await;
		assert_eq!(flag(&flags, &again), Some(true), "thread receipt: {flags:?}");

		let last = send(&router, &room, HELPER_TOKEN, "last", thread_reply(&root)).await;
		let newest = send(&router, &room, HELPER_TOKEN, "newest", text("Newest")).await;
		receipt(&router, &room, "m.read.private", &newest, json!({})).await;
		let flags = read_flags(&router).await;
		assert_eq!(flag(&flags, &last), Some(true), "private receipt: {flags:?}");

		Ok(())
	})
}

/// A thread the reader read in the thread stays read after an unthreaded
/// receipt on an older main-timeline event, as its sync count does: read by a
/// private thread receipt, an own reply, or a public thread receipt, then an
/// unthreaded receipt, public or private, by `/receipt` or `/read_markers`.
#[test]
fn thread_reads_survive_older_unthreaded_receipt() -> Result {
	let harness = Harness::new("mindroom_notifications_thread_reads_kept", [QUIET.to_owned()])?;

	harness.with_services(async |services| {
		let (router, _guard) = serve(services).await?;

		for case in ["private", "own", "read_markers", "public", "own_private", "private_private"]
		{
			let room = create_room(&router).await;
			let path = format!("/rooms/{room}/join");
			request(&router, Method::POST, &path, HELPER_TOKEN, json!({})).await;
			let older =
				send(&router, &room, HELPER_TOKEN, &format!("{case}-older"), text("Older")).await;
			let root =
				send(&router, &room, HELPER_TOKEN, &format!("{case}-root"), text("Root")).await;
			let reply =
				send(&router, &room, HELPER_TOKEN, &format!("{case}-reply"), thread_reply(&root))
					.await;

			let in_thread = json!({"thread_id": root});
			match case {
				| "private" | "read_markers" | "private_private" =>
					receipt(&router, &room, "m.read.private", &reply, in_thread).await,
				| "public" => receipt(&router, &room, "m.read", &reply, in_thread).await,
				| _ => {
					let txn = format!("{case}-own");
					send(&router, &room, READER_TOKEN, &txn, thread_reply(&root)).await;
				},
			}

			match case {
				| "read_markers" => {
					let path = format!("/rooms/{room}/read_markers");
					request(&router, Method::POST, &path, READER_TOKEN, json!({"m.read": older}))
						.await;
				},
				| "own_private" | "private_private" =>
					receipt(&router, &room, "m.read.private", &older, json!({})).await,
				| _ => receipt(&router, &room, "m.read", &older, json!({})).await,
			}

			let flags = read_flags(&router).await;
			assert_eq!(flag(&flags, &reply), Some(true), "{case}: {flags:?}");
			assert_eq!(thread_count(&router, &room, &root).await, 0, "{case}: sync");
		}

		Ok(())
	})
}

/// Reads that do not cover a thread reply leave it unread, as its sync count
/// does: another member's later reply in the thread and unthreaded receipt, the
/// reader's receipt in another thread or room, and the reader's reaction to the
/// root. A later notice in the thread, which does not notify, a later reply
/// that only highlights, and a later main-timeline reply to the root do not
/// count as replies after it. A reply that only highlights stays unread while
/// the thread's highlight count holds it. Fetched one entry per page, every
/// entry reads the same.
#[test]
fn other_reads_leave_thread_replies_unread() -> Result {
	let harness = Harness::new("mindroom_notifications_other_reads", [QUIET.to_owned()])?;

	harness.with_services(async |services| {
		let (router, _guard) = serve(services).await?;

		let room = create_room(&router).await;
		let path = format!("/rooms/{room}/join");
		request(&router, Method::POST, &path, HELPER_TOKEN, json!({})).await;
		let root_a = send(&router, &room, HELPER_TOKEN, "root-a", text("First")).await;
		let root_b = send(&router, &room, HELPER_TOKEN, "root-b", text("Second")).await;
		let early_b = send(&router, &room, HELPER_TOKEN, "b-0", thread_reply(&root_b)).await;
		receipt(&router, &room, "m.read", &early_b, json!({"thread_id": root_b})).await;
		let reply_b = send(&router, &room, HELPER_TOKEN, "b-1", thread_reply(&root_b)).await;
		send(&router, &room, HELPER_TOKEN, "b-2", thread_reply(&root_b)).await;
		let other = create_room(&router).await;
		let path = format!("/rooms/{other}/join");
		request(&router, Method::POST, &path, HELPER_TOKEN, json!({})).await;
		let root_e = send(&router, &other, HELPER_TOKEN, "root-e", text("Elsewhere")).await;
		let early_e = send(&router, &other, HELPER_TOKEN, "e-0", thread_reply(&root_e)).await;
		receipt(&router, &other, "m.read", &early_e, json!({"thread_id": root_e})).await;
		let reply_e = send(&router, &other, HELPER_TOKEN, "e-1", thread_reply(&root_e)).await;
		let reply_a = send(&router, &room, HELPER_TOKEN, "a-1", thread_reply(&root_a)).await;
		let root_c = send(&router, &room, HELPER_TOKEN, "root-c", text("Third")).await;
		let reply_c = send(&router, &room, HELPER_TOKEN, "c-1", thread_reply(&root_c)).await;
		let mut notice = thread_reply(&root_c);
		notice["msgtype"] = "m.notice".into();
		send(&router, &room, HELPER_TOKEN, "c-notice", notice).await;
		let rule = json!({
			"conditions": [{"kind": "event_match", "key": "content.body", "pattern": "quiet"}],
			"actions": [{"set_tweak": "highlight"}],
		});
		// Loading the rules stores the defaults the account was created without.
		request(&router, Method::GET, "/pushrules/global/", READER_TOKEN, Value::Null).await;
		let path = "/pushrules/global/override/quiet";
		request(&router, Method::PUT, path, READER_TOKEN, rule).await;
		let mut quiet = thread_reply(&root_c);
		quiet["body"] = "quiet".into();
		send(&router, &room, HELPER_TOKEN, "c-quiet", quiet).await;
		let root_d = send(&router, &room, HELPER_TOKEN, "root-d", text("Fourth")).await;
		let early_d = send(&router, &room, HELPER_TOKEN, "d-0", thread_reply(&root_d)).await;
		receipt(&router, &room, "m.read", &early_d, json!({"thread_id": root_d})).await;
		let mut quiet = thread_reply(&root_d);
		quiet["body"] = "quiet".into();
		let quiet_d = send(&router, &room, HELPER_TOKEN, "d-quiet", quiet).await;
		let flags = read_flags(&router).await;
		assert_eq!(flag(&flags, &quiet_d), Some(false), "highlight only: {flags:?}");
		assert_eq!(thread_counts(&router, &room, &root_d).await, (0, 1), "sync");
		assert_eq!(paged_flags(&router).await, flags, "highlight only, one entry per page");
		let reply_d = send(&router, &room, HELPER_TOKEN, "d-1", thread_reply(&root_d)).await;
		let main_reply = json!({
			"msgtype": "m.text",
			"body": "Answer in the room",
			"m.relates_to": {"m.in_reply_to": {"event_id": root_c}},
		});
		send(&router, &room, HELPER_TOKEN, "c-main", main_reply).await;

		let path = format!("/rooms/{room}/receipt/m.read/{reply_a}");
		request(&router, Method::POST, &path, HELPER_TOKEN, json!({})).await;
		receipt(&router, &room, "m.read", &reply_a, json!({"thread_id": root_a})).await;
		let path = format!("/rooms/{room}/send/m.reaction/reaction");
		let reaction = json!({
			"m.relates_to": {"rel_type": "m.annotation", "event_id": root_b, "key": "+1"},
		});
		request(&router, Method::PUT, &path, READER_TOKEN, reaction).await;

		let flags = read_flags(&router).await;
		assert_eq!(flag(&flags, &reply_a), Some(true), "{flags:?}");
		assert_eq!(flag(&flags, &reply_b), Some(false), "{flags:?}");
		assert_eq!(thread_count(&router, &room, &root_b).await, 2, "sync");
		assert_eq!(flag(&flags, &reply_c), Some(false), "{flags:?}");
		assert_eq!(thread_count(&router, &room, &root_c).await, 1, "sync");
		assert_eq!(flag(&flags, &early_b), Some(true), "{flags:?}");
		assert_eq!(flag(&flags, &early_e), Some(true), "{flags:?}");
		assert_eq!(flag(&flags, &reply_e), Some(false), "{flags:?}");
		assert_eq!(thread_count(&router, &other, &root_e).await, 1, "sync");
		assert_eq!(flag(&flags, &early_d), Some(true), "{flags:?}");
		assert_eq!(flag(&flags, &quiet_d), Some(false), "{flags:?}");
		assert_eq!(flag(&flags, &reply_d), Some(false), "{flags:?}");
		assert_eq!(thread_counts(&router, &room, &root_d).await, (1, 1), "sync");
		assert_eq!(paged_flags(&router).await, flags, "one entry per page");

		Ok(())
	})
}

/// A finished stream counts in its thread by its terminal edit, which relates
/// to the reply rather than the root. When the reader reads the thread while a
/// second reply streams, sync counts only the second, and the Inbox shows the
/// first finished reply read and the second unread.
#[test]
fn finished_streams_follow_the_thread_count() -> Result {
	let harness = Harness::new("mindroom_notifications_streams", [QUIET.to_owned()])?;

	harness.with_services(async |services| {
		let (router, _guard) = serve(services).await?;

		let room = create_room(&router).await;
		let path = format!("/rooms/{room}/join");
		request(&router, Method::POST, &path, HELPER_TOKEN, json!({})).await;
		let root = send(&router, &room, HELPER_TOKEN, "root", text("Question")).await;
		let first_a = send(&router, &room, HELPER_TOKEN, "a", streaming(&root)).await;
		let done_a = send(&router, &room, HELPER_TOKEN, "a-done", finished(&first_a)).await;
		let first_b = send(&router, &room, HELPER_TOKEN, "b", streaming(&root)).await;
		receipt(&router, &room, "m.read", &first_b, json!({"thread_id": root})).await;
		let done_b = send(&router, &room, HELPER_TOKEN, "b-done", finished(&first_b)).await;

		let flags = read_flags(&router).await;
		assert_eq!(flag(&flags, &done_a), Some(true), "{flags:?}");
		assert_eq!(flag(&flags, &done_b), Some(false), "{flags:?}");
		assert_eq!(thread_count(&router, &room, &root).await, 1, "sync");

		Ok(())
	})
}

/// An unthreaded receipt whose recount passes its limit of 4,096 events reads
/// every thread through the present in the sync counts, and the Inbox agrees.
#[test]
fn recount_past_limit_reads_thread_replies() -> Result {
	let harness = Harness::new("mindroom_notifications_recount_limit", [QUIET.to_owned()])?;

	harness.with_services(async |services| {
		let (router, _guard) = serve(services).await?;

		let room = create_room(&router).await;
		let path = format!("/rooms/{room}/join");
		request(&router, Method::POST, &path, HELPER_TOKEN, json!({})).await;
		let early = send(&router, &room, HELPER_TOKEN, "early", text("Early")).await;
		let root = send(&router, &room, HELPER_TOKEN, "root", text("Question")).await;
		let reply = send(&router, &room, HELPER_TOKEN, "reply", thread_reply(&root)).await;
		for i in 0..4100 {
			let path = format!("/rooms/{room}/send/org.example.filler/filler-{i}");
			request(&router, Method::PUT, &path, HELPER_TOKEN, json!({"n": i})).await;
		}
		receipt(&router, &room, "m.read", &early, json!({})).await;

		let flags = read_flags(&router).await;
		assert_eq!(flag(&flags, &reply), Some(true), "{flags:?}");
		assert_eq!(thread_count(&router, &room, &root).await, 0, "sync");

		Ok(())
	})
}

/// A receipt reads up to the event it names, so the main-timeline entries after
/// an older receipt stay unread in `/notifications`, as the room's sync count
/// holds them: unthreaded or `"main"`, public or private, by `/receipt` or
/// `/read_markers`. A thread reply after it keeps its thread's count, a later
/// message is unread with them, and fetched one entry per page every entry
/// reads the same.
#[test]
fn older_receipt_leaves_newer_main_entries_unread() -> Result {
	let harness = Harness::new("mindroom_notifications_older_receipt", [QUIET.to_owned()])?;

	harness.with_services(async |services| {
		let (router, _guard) = serve(services).await?;

		for case in ["unthreaded", "main", "private", "read_markers"] {
			let room = create_room(&router).await;
			let path = format!("/rooms/{room}/join");
			request(&router, Method::POST, &path, HELPER_TOKEN, json!({})).await;
			let txn = |name: &str| format!("{case}-{name}");
			let first = send(&router, &room, HELPER_TOKEN, &txn("first"), text("First")).await;
			let read = send(&router, &room, HELPER_TOKEN, &txn("read"), text("Second")).await;
			let root = send(&router, &room, HELPER_TOKEN, &txn("root"), text("Question")).await;
			let reply =
				send(&router, &room, HELPER_TOKEN, &txn("reply"), thread_reply(&root)).await;
			let last = send(&router, &room, HELPER_TOKEN, &txn("last"), text("Last")).await;

			match case {
				| "unthreaded" => receipt(&router, &room, "m.read", &read, json!({})).await,
				| "main" => {
					let main = json!({"thread_id": "main"});
					receipt(&router, &room, "m.read", &read, main).await;
				},
				| "private" => receipt(&router, &room, "m.read.private", &read, json!({})).await,
				| _ => {
					let path = format!("/rooms/{room}/read_markers");
					request(&router, Method::POST, &path, READER_TOKEN, json!({"m.read": read}))
						.await;
				},
			}

			let flags = read_flags(&router).await;
			let events = [&first, &read, &root, &reply, &last];
			let got: Vec<_> = events
				.iter()
				.map(|event| flag(&flags, event))
				.collect();
			let expected = [Some(true), Some(true), Some(false), Some(false), Some(false)];
			assert_eq!(got, expected, "{case}: {flags:?}");
			assert_eq!(room_counts(&router, &room).await, (2, 0), "{case}: sync");
			assert_eq!(thread_count(&router, &room, &root).await, 1, "{case}: sync");

			let later = send(&router, &room, HELPER_TOKEN, &txn("later"), text("Later")).await;
			let flags = read_flags(&router).await;
			assert_eq!(flag(&flags, &read), Some(true), "{case} later: {flags:?}");
			assert_eq!(flag(&flags, &root), Some(false), "{case} later: {flags:?}");
			assert_eq!(flag(&flags, &later), Some(false), "{case} later: {flags:?}");
			assert_eq!(room_counts(&router, &room).await, (3, 0), "{case} later: sync");
			assert_eq!(paged_flags(&router).await, flags, "{case}: one entry per page");
		}

		Ok(())
	})
}

/// A main-timeline message that only highlights stays unread after a receipt
/// on an older message while the room's highlight count holds it, also once a
/// later message notifies.
#[test]
fn highlight_only_main_entry_stays_unread() -> Result {
	let harness = Harness::new("mindroom_notifications_main_highlight", [QUIET.to_owned()])?;

	harness.with_services(async |services| {
		let (router, _guard) = serve(services).await?;

		let rule = json!({
			"conditions": [{"kind": "event_match", "key": "content.body", "pattern": "quiet"}],
			"actions": [{"set_tweak": "highlight"}],
		});
		// Loading the rules stores the defaults the account was created without.
		request(&router, Method::GET, "/pushrules/global/", READER_TOKEN, Value::Null).await;
		let path = "/pushrules/global/override/quiet";
		request(&router, Method::PUT, path, READER_TOKEN, rule).await;

		let room = create_room(&router).await;
		let path = format!("/rooms/{room}/join");
		request(&router, Method::POST, &path, HELPER_TOKEN, json!({})).await;
		let first = send(&router, &room, HELPER_TOKEN, "first", text("First")).await;
		let quiet = send(&router, &room, HELPER_TOKEN, "quiet", text("quiet")).await;
		receipt(&router, &room, "m.read", &first, json!({})).await;

		let flags = read_flags(&router).await;
		assert_eq!(flag(&flags, &first), Some(true), "{flags:?}");
		assert_eq!(flag(&flags, &quiet), Some(false), "{flags:?}");
		assert_eq!(room_counts(&router, &room).await, (0, 1), "sync");

		let later = send(&router, &room, HELPER_TOKEN, "later", text("Later")).await;
		let flags = read_flags(&router).await;
		assert_eq!(flag(&flags, &first), Some(true), "later: {flags:?}");
		assert_eq!(flag(&flags, &quiet), Some(false), "later: {flags:?}");
		assert_eq!(flag(&flags, &later), Some(false), "later: {flags:?}");
		assert_eq!(room_counts(&router, &room).await, (1, 1), "later: sync");

		Ok(())
	})
}

/// A redacted thread reply no longer names its thread. Taken for a
/// main-timeline event, it would stand in for one the room's count holds and
/// show that one read. A streamed reply notifies by its terminal edit, which is
/// redacted with the reply.
#[test]
fn redacted_reply_leaves_main_entries_unread() -> Result {
	let harness = Harness::new("mindroom_notifications_redacted_reply", [QUIET.to_owned()])?;

	harness.with_services(async |services| {
		let (router, _guard) = serve(services).await?;

		let room = create_room(&router).await;
		let path = format!("/rooms/{room}/join");
		request(&router, Method::POST, &path, HELPER_TOKEN, json!({})).await;
		let older = send(&router, &room, HELPER_TOKEN, "older", text("Older")).await;
		let root = send(&router, &room, HELPER_TOKEN, "root", text("Question")).await;
		let reply = send(&router, &room, HELPER_TOKEN, "reply", thread_reply(&root)).await;
		let first = send(&router, &room, HELPER_TOKEN, "first", streaming(&root)).await;
		send(&router, &room, HELPER_TOKEN, "done", finished(&first)).await;
		for (event, txn) in [(&reply, "redact-reply"), (&first, "redact-first")] {
			let path = format!("/rooms/{room}/redact/{event}/{txn}");
			request(&router, Method::PUT, &path, HELPER_TOKEN, json!({})).await;
		}

		let flags = read_flags(&router).await;
		assert_eq!(flag(&flags, &older), Some(false), "{flags:?}");
		assert_eq!(flag(&flags, &root), Some(false), "{flags:?}");
		assert_eq!(room_counts(&router, &room).await, (2, 0), "sync");

		Ok(())
	})
}

/// Redaction strips a thread event's `m.relates_to`, so its redaction and an
/// event related to it, such as a poll end, belong to the main timeline
/// afterwards, where MSC3771 clients such as matrix-js-sdk file them. A `"main"`
/// receipt on either is accepted and reads the main timeline, in the sync count
/// and in the Inbox.
#[test]
fn main_receipt_after_thread_redaction_reads_main() -> Result {
	let harness = Harness::new("mindroom_notifications_redaction_main", [QUIET.to_owned()])?;

	harness.with_services(async |services| {
		let (router, _guard) = serve(services).await?;

		for case in ["redaction", "end"] {
			let room = create_room(&router).await;
			let path = format!("/rooms/{room}/join");
			request(&router, Method::POST, &path, HELPER_TOKEN, json!({})).await;
			let txn = |name: &str| format!("{case}-{name}");
			let older = send(&router, &room, HELPER_TOKEN, &txn("older"), text("Older")).await;
			let root = send(&router, &room, HELPER_TOKEN, &txn("root"), text("Question")).await;
			let kind = "org.matrix.msc3381.poll.start";
			let path = format!("/rooms/{room}/send/{kind}/{}", txn("start"));
			let start =
				event_id(&request(&router, Method::PUT, &path, HELPER_TOKEN, poll(&root)).await);
			let end = json!({
				"org.matrix.msc3381.poll.end": {},
				"body": "Ended",
				"m.relates_to": {"rel_type": "m.reference", "event_id": start},
			});
			let kind = "org.matrix.msc3381.poll.end";
			let path = format!("/rooms/{room}/send/{kind}/{}", txn("end"));
			let end = event_id(&request(&router, Method::PUT, &path, HELPER_TOKEN, end).await);
			let path = format!("/rooms/{room}/redact/{start}/{}", txn("redact"));
			let redaction =
				event_id(&request(&router, Method::PUT, &path, HELPER_TOKEN, json!({})).await);
			assert_eq!(room_counts(&router, &room).await, (2, 0), "{case}: sync");

			let target = if case == "end" { &end } else { &redaction };
			receipt(&router, &room, "m.read", target, json!({"thread_id": "main"})).await;
			assert_eq!(room_counts(&router, &room).await, (0, 0), "{case}: main receipt");
			let flags = read_flags(&router).await;
			for event in [&older, &root, &end] {
				assert_eq!(flag(&flags, event), Some(true), "{case}: {flags:?}");
			}
		}

		Ok(())
	})
}

async fn serve(services: Arc<Services>) -> Result<(Router, Guard)> {
	for (user, token) in [
		(user_id!("@reader:localhost"), READER_TOKEN),
		(user_id!("@helper:localhost"), HELPER_TOKEN),
	] {
		services
			.users
			.create(user, Some("password"), None)
			.await?;
		services
			.users
			.create_device(user, None, (Some(token), None), None, None, None)
			.await?;
	}

	let (state, guard) = tuwunel_api::router::state::create(services.clone());
	let router = tuwunel_api::router::build(Router::new(), &services.server).with_state(state);

	Ok((router, guard))
}

/// The reader's sync `(notification, highlight)` counts for the main timeline
/// of `room`.
async fn room_counts(router: &Router, room: &RoomId) -> (u64, u64) {
	let sync = sync(router).await;

	counts(&sync["rooms"]["join"][room.as_str()]["unread_notifications"])
}

/// The reader's sync count for the thread of `root`; a thread left out of
/// `unread_thread_notifications` has none.
async fn thread_count(router: &Router, room: &RoomId, root: &OwnedEventId) -> u64 {
	thread_counts(router, room, root).await.0
}

/// The reader's sync `(notification, highlight)` counts for the thread of
/// `root`; a thread left out of `unread_thread_notifications` has none.
async fn thread_counts(router: &Router, room: &RoomId, root: &OwnedEventId) -> (u64, u64) {
	let sync = sync(router).await;

	counts(&sync["rooms"]["join"][room.as_str()]["unread_thread_notifications"][root.as_str()])
}

/// A sync with the thread counts apart from the main timeline's.
async fn sync(router: &Router) -> Value {
	let filter = r#"{"room":{"timeline":{"unread_thread_notifications":true}}}"#;
	let filter: String = url::form_urlencoded::byte_serialize(filter.as_bytes()).collect();
	let path = format!("/sync?timeout=0&filter={filter}");

	request(router, Method::GET, &path, READER_TOKEN, Value::Null).await
}

fn counts(counts: &Value) -> (u64, u64) {
	(
		counts["notification_count"].as_u64().unwrap_or(0),
		counts["highlight_count"].as_u64().unwrap_or(0),
	)
}

async fn read_flags(router: &Router) -> Vec<(String, bool)> {
	let response =
		request(router, Method::GET, "/notifications", READER_TOKEN, Value::Null).await;

	entries(&response)
}

/// The flags fetched one entry per page, following `next_token`.
async fn paged_flags(router: &Router) -> Vec<(String, bool)> {
	let (mut flags, mut from) = (Vec::new(), String::new());
	loop {
		let path = format!("/notifications?limit=1{from}");
		let page = request(router, Method::GET, &path, READER_TOKEN, Value::Null).await;
		flags.extend(entries(&page));
		let Some(token) = page["next_token"].as_str() else {
			return flags;
		};
		from = format!("&from={token}");
	}
}

fn entries(response: &Value) -> Vec<(String, bool)> {
	response["notifications"]
		.as_array()
		.expect("notifications")
		.iter()
		.map(|notification| {
			let event_id = notification["event"]["event_id"].as_str();
			let read = notification["read"].as_bool();
			(event_id.expect("event id").to_owned(), read.expect("read flag"))
		})
		.collect()
}

fn flag(flags: &[(String, bool)], event: &OwnedEventId) -> Option<bool> {
	flags
		.iter()
		.find(|(event_id, _)| event_id == event.as_str())
		.map(|(_, read)| *read)
}

fn text(body: &str) -> Value { json!({"msgtype": "m.text", "body": body}) }

/// The first event of a reply streaming in the thread of `root`.
fn streaming(root: &OwnedEventId) -> Value {
	json!({
		"msgtype": "m.text",
		"body": "Thinking",
		"io.mindroom.stream_status": "streaming",
		"m.relates_to": {"rel_type": "m.thread", "event_id": root},
	})
}

/// The terminal edit that finishes the stream started by `first`.
fn finished(first: &OwnedEventId) -> Value {
	json!({
		"msgtype": "m.text",
		"body": "* Answer",
		"m.new_content": {"msgtype": "m.text", "body": "Answer"},
		"io.mindroom.stream_status": "completed",
		"m.relates_to": {"rel_type": "m.replace", "event_id": first},
	})
}

/// An unstable poll start in the thread of `root`.
fn poll(root: &OwnedEventId) -> Value {
	json!({
		"org.matrix.msc3381.poll.start": {
			"question": {"body": "Lunch?"},
			"kind": "org.matrix.msc3381.poll.disclosed",
			"max_selections": 1,
			"answers": [{"id": "yes", "body": "Yes"}, {"id": "no", "body": "No"}],
		},
		"body": "Lunch?",
		"m.relates_to": {"rel_type": "m.thread", "event_id": root},
	})
}

fn thread_reply(root: &OwnedEventId) -> Value {
	json!({
		"msgtype": "m.text",
		"body": "Reply",
		"m.relates_to": {"rel_type": "m.thread", "event_id": root},
	})
}

async fn receipt(router: &Router, room: &RoomId, kind: &str, event: &OwnedEventId, body: Value) {
	let path = format!("/rooms/{room}/receipt/{kind}/{event}");
	request(router, Method::POST, &path, READER_TOKEN, body).await;
}

async fn create_room(router: &Router) -> OwnedRoomId {
	let body = json!({"preset": "public_chat"});
	let response = request(router, Method::POST, "/createRoom", READER_TOKEN, body).await;

	response["room_id"]
		.as_str()
		.expect("created room id")
		.try_into()
		.expect("valid room id")
}

async fn send(
	router: &Router,
	room: &RoomId,
	token: &str,
	txn: &str,
	content: Value,
) -> OwnedEventId {
	let path = format!("/rooms/{room}/send/m.room.message/{txn}");

	event_id(&request(router, Method::PUT, &path, token, content).await)
}

fn event_id(response: &Value) -> OwnedEventId {
	response["event_id"]
		.as_str()
		.expect("sent event id")
		.try_into()
		.expect("valid event id")
}

async fn request(router: &Router, method: Method, path: &str, token: &str, body: Value) -> Value {
	let body = if body.is_null() {
		Body::empty()
	} else {
		Body::from(body.to_string())
	};
	let request = Request::builder()
		.method(method)
		.uri(format!("/_matrix/client/v3{path}"))
		.header(header::AUTHORIZATION, format!("Bearer {token}"))
		.header(header::CONTENT_TYPE, "application/json")
		.header("X-Forwarded-For", "127.0.0.1")
		.body(body)
		.expect("valid client request");
	let response = router
		.clone()
		.oneshot(request)
		.await
		.expect("client response");
	let status = response.status();
	let body = axum::body::to_bytes(response.into_body(), 1 << 20)
		.await
		.expect("read response");
	let body: Value = serde_json::from_slice(&body).expect("JSON response");
	assert_eq!(status, StatusCode::OK, "{path}: {body}");
	body
}
