//! Every sync that carries a room lists all its threads with notifications.

#![cfg(test)]

mod support;

use axum::{Router, body::Body};
use serde_json::{Value, json};
use tower::ServiceExt;
use tuwunel_core::{
	Result,
	http::{Method, Request, StatusCode, header},
	ruma::{OwnedEventId, OwnedRoomId, RoomId, user_id},
};

use self::support::Harness;

const READER_TOKEN: &str = "thread-counts-reader-test-token-0123456789abcdef";
const HELPER_TOKEN: &str = "thread-counts-helper-test-token-0123456789abcdef";

const THREADED: &str = r#"{"room":{"timeline":{"unread_thread_notifications":true}}}"#;
const UNTHREADED: &str = r#"{"room":{"timeline":{}}}"#;
const THREADED_LAZY_MEMBERS: &str = concat!(
	r#"{"room":{"state":{"lazy_load_members":true},"#,
	r#""timeline":{"unread_thread_notifications":true,"lazy_load_members":true}}}"#,
);

/// MSC3773: `unread_thread_notifications` holds the counts of each thread in
/// the room, and a client takes a thread left out of it as read. A sync that
/// carries the room must therefore list every thread that has notifications on
/// any round: after a `"main"` receipt, another member's typing or receipt, or
/// the reader's receipt in one thread, and under a filter that lazy loads
/// members. A thread read down to zero is left out, an unthreaded receipt
/// reads them all, and a room with nothing new still stays out of the response.
#[test]
fn sync_lists_every_thread_count() -> Result {
	let harness = Harness::new("mindroom_sync_thread_counts", [])?;

	harness.with_services(async |services| {
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

		let (state, _guard) = tuwunel_api::router::state::create(services.clone());
		let router =
			tuwunel_api::router::build(Router::new(), &services.server).with_state(state);

		let room = create_room(&router, READER_TOKEN).await;
		let path = format!("/rooms/{room}/join");
		request(&router, Method::POST, &path, HELPER_TOKEN, json!({})).await;

		let first = send(&router, &room, READER_TOKEN, "first", text("First question")).await;
		let second = send(&router, &room, READER_TOKEN, "second", text("Second question")).await;
		let first_reply =
			send(&router, &room, HELPER_TOKEN, "first-reply", thread_reply(&first, &[])).await;
		let mention = ["@reader:localhost"];
		send(&router, &room, HELPER_TOKEN, "second-reply", thread_reply(&second, &mention)).await;

		// The mention in the second thread also counts as a highlight.
		let initial = sync(&router, THREADED, None).await;
		assert_eq!(counts(&initial, &room, &first), Some((1, 0)), "initial: {initial}");
		assert_eq!(counts(&initial, &room, &second), Some((1, 1)), "initial: {initial}");

		// A client that does not ask for thread counts gets them summed into the
		// room counts.
		let summed = sync(&router, UNTHREADED, None).await;
		assert_eq!(room_counts(&summed, &room), (Some(2), Some(1)), "summed: {summed}");
		assert!(!has_thread_map(&summed, &room), "summed: {summed}");

		// A `"main"` receipt, sent while the main timeline is shown, reads no
		// thread.
		receipt(&router, &room, READER_TOKEN, &second, json!({"thread_id": "main"})).await;
		let main = sync(&router, THREADED, Some(&initial)).await;
		assert_eq!(counts(&main, &room, &first), Some((1, 0)), "main receipt: {main}");
		assert_eq!(counts(&main, &room, &second), Some((1, 1)), "main receipt: {main}");

		// Another member typing carries the room without reading anything.
		let path = format!("/rooms/{room}/typing/@helper:localhost");
		let body = json!({"typing": true, "timeout": 30_000});
		request(&router, Method::PUT, &path, HELPER_TOKEN, body).await;
		let typing = sync(&router, THREADED, Some(&main)).await;
		assert_eq!(counts(&typing, &room, &first), Some((1, 0)), "typing: {typing}");
		assert_eq!(counts(&typing, &room, &second), Some((1, 1)), "typing: {typing}");
		// Stop typing, so the 30 s typing timeout cannot bring the room back into
		// the quiet round below on a slow run.
		request(&router, Method::PUT, &path, HELPER_TOKEN, json!({"typing": false})).await;

		// The reader opens the first thread. Read down to zero, it is left out
		// of the map, which the client takes as read; the second is still
		// unread.
		receipt(&router, &room, READER_TOKEN, &first_reply, json!({"thread_id": first})).await;
		let read = sync(&router, THREADED, Some(&typing)).await;
		assert_eq!(counts(&read, &room, &first), None, "read thread: {read}");
		assert_eq!(client_counts(&read, &room, &first), Some((0, 0)), "read thread: {read}");
		assert_eq!(counts(&read, &room, &second), Some((1, 1)), "unread thread: {read}");

		// Another member's receipt carries the room without moving the reader's
		// read positions.
		receipt(&router, &room, HELPER_TOKEN, &second, json!({})).await;
		let other = sync(&router, THREADED, Some(&read)).await;
		assert_eq!(counts(&other, &room, &first), None, "read thread: {other}");
		assert_eq!(client_counts(&other, &room, &first), Some((0, 0)), "read thread: {other}");
		assert_eq!(counts(&other, &room, &second), Some((1, 1)), "unread thread: {other}");

		// Activity in another room moves the stream, so this round is built,
		// but nothing changed in this room.
		let elsewhere = create_room(&router, HELPER_TOKEN).await;
		send(&router, &elsewhere, HELPER_TOKEN, "elsewhere", text("Meanwhile")).await;
		let quiet = sync(&router, THREADED, Some(&other)).await;
		assert!(joined(&quiet, &room).is_null(), "a quiet room stays out: {quiet}");

		// A new reply, then another member's receipt under a filter that lazy
		// loads members.
		let again =
			send(&router, &room, HELPER_TOKEN, "first-again", thread_reply(&first, &[])).await;
		let busy = sync(&router, THREADED, Some(&quiet)).await;
		assert_eq!(counts(&busy, &room, &first), Some((1, 0)), "new reply: {busy}");
		receipt(&router, &room, HELPER_TOKEN, &again, json!({})).await;
		let lazy = sync(&router, THREADED_LAZY_MEMBERS, Some(&busy)).await;
		assert_eq!(counts(&lazy, &room, &first), Some((1, 0)), "lazy members: {lazy}");
		assert_eq!(counts(&lazy, &room, &second), Some((1, 1)), "lazy members: {lazy}");

		// An unthreaded receipt reads every thread: the room carries no map, and
		// the client takes every thread as read.
		receipt(&router, &room, READER_TOKEN, &again, json!({})).await;
		let all = sync(&router, THREADED, Some(&lazy)).await;
		assert_eq!(room_counts(&all, &room), (Some(0), Some(0)), "unthreaded receipt: {all}");
		assert!(!has_thread_map(&all, &room), "unthreaded receipt: {all}");
		assert_eq!(client_counts(&all, &room, &first), Some((0, 0)), "unthreaded receipt: {all}");
		assert_eq!(
			client_counts(&all, &room, &second),
			Some((0, 0)),
			"unthreaded receipt: {all}"
		);

		Ok(())
	})
}

fn joined<'a>(sync: &'a Value, room: &RoomId) -> &'a Value {
	&sync["rooms"]["join"][room.as_str()]
}

fn has_thread_map(sync: &Value, room: &RoomId) -> bool {
	!joined(sync, room)["unread_thread_notifications"].is_null()
}

fn room_counts(sync: &Value, room: &RoomId) -> (Option<u64>, Option<u64>) {
	let counts = &joined(sync, room)["unread_notifications"];

	(counts["notification_count"].as_u64(), counts["highlight_count"].as_u64())
}

/// The `(notification, highlight)` counts the map lists for a thread.
fn counts(sync: &Value, room: &RoomId, root: &OwnedEventId) -> Option<(u64, u64)> {
	let thread = &joined(sync, room)["unread_thread_notifications"][root.as_str()];

	Some((thread["notification_count"].as_u64()?, thread["highlight_count"].as_u64()?))
}

/// The counts a client keeps for a thread after this sync, as matrix-js-sdk
/// applies it in this unencrypted room (in an encrypted room it resets the
/// total but keeps highlights it calculated itself): the listed ones, zero for a thread left out of a room in the
/// response, or `None` when the room is not in the response and nothing
/// changes.
fn client_counts(sync: &Value, room: &RoomId, root: &OwnedEventId) -> Option<(u64, u64)> {
	joined(sync, room)
		.is_object()
		.then(|| counts(sync, room, root).unwrap_or((0, 0)))
}

fn text(body: &str) -> Value { json!({"msgtype": "m.text", "body": body}) }

fn thread_reply(root: &OwnedEventId, mentions: &[&str]) -> Value {
	json!({
		"msgtype": "m.text",
		"body": "An answer",
		"m.relates_to": {"rel_type": "m.thread", "event_id": root},
		"m.mentions": {"user_ids": mentions},
	})
}

async fn receipt(router: &Router, room: &RoomId, token: &str, event: &OwnedEventId, body: Value) {
	let path = format!("/rooms/{room}/receipt/m.read/{event}");
	request(router, Method::POST, &path, token, body).await;
}

async fn sync(router: &Router, filter: &str, since: Option<&Value>) -> Value {
	let filter: String = url::form_urlencoded::byte_serialize(filter.as_bytes()).collect();
	let since = since.map_or_else(String::new, |sync| {
		format!("&since={}", sync["next_batch"].as_str().expect("next_batch"))
	});

	request(
		router,
		Method::GET,
		&format!("/sync?timeout=0&filter={filter}{since}"),
		READER_TOKEN,
		Value::Null,
	)
	.await
}

async fn create_room(router: &Router, token: &str) -> OwnedRoomId {
	let response =
		request(router, Method::POST, "/createRoom", token, json!({"preset": "public_chat"}))
			.await;

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
