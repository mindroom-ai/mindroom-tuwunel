//! `/notifications` marks a thread reply read only by a read that covers its
//! thread.

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
/// unthreaded `m.read` by `/receipt` or `/read_markers`. An own reply also
/// outlives an unthreaded private receipt, which drops the private thread
/// marker.
#[test]
fn thread_reads_survive_older_unthreaded_receipt() -> Result {
	let harness = Harness::new("mindroom_notifications_thread_reads_kept", [QUIET.to_owned()])?;

	harness.with_services(async |services| {
		let (router, _guard) = serve(services).await?;

		for case in ["private", "own", "read_markers", "public", "own_private"] {
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
				| "private" | "read_markers" =>
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
				| "own_private" =>
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
/// reader's receipt in another thread, and the reader's reaction to the root.
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
		let reply_b = send(&router, &room, HELPER_TOKEN, "b-1", thread_reply(&root_b)).await;
		send(&router, &room, HELPER_TOKEN, "b-2", thread_reply(&root_b)).await;
		let reply_a = send(&router, &room, HELPER_TOKEN, "a-1", thread_reply(&root_a)).await;

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

/// The reader's sync count for the thread of `root`; a thread left out of
/// `unread_thread_notifications` has none.
async fn thread_count(router: &Router, room: &RoomId, root: &OwnedEventId) -> u64 {
	let filter = r#"{"room":{"timeline":{"unread_thread_notifications":true}}}"#;
	let filter: String = url::form_urlencoded::byte_serialize(filter.as_bytes()).collect();
	let path = format!("/sync?timeout=0&filter={filter}");
	let sync = request(router, Method::GET, &path, READER_TOKEN, Value::Null).await;

	sync["rooms"]["join"][room.as_str()]["unread_thread_notifications"][root.as_str()]
		["notification_count"]
		.as_u64()
		.unwrap_or(0)
}

async fn read_flags(router: &Router) -> Vec<(String, bool)> {
	let response =
		request(router, Method::GET, "/notifications", READER_TOKEN, Value::Null).await;

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
	let response = request(router, Method::PUT, &path, token, content).await;

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
