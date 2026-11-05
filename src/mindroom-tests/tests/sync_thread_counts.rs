//! Every sync that carries a room lists the counts of all its threads.

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

/// MSC3773: `unread_thread_notifications` holds the count of each thread in
/// the room, and a client takes a thread left out of it as read. A sync that
/// carries the room must therefore list every thread that has a count, also
/// on a round with only a receipt in it, whether it is the reader's receipt in
/// one thread or another member's receipt. A room with nothing new still stays
/// out of the response.
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
			send(&router, &room, HELPER_TOKEN, "first-reply", thread_reply(&first)).await;
		send(&router, &room, HELPER_TOKEN, "second-reply", thread_reply(&second)).await;

		let initial = sync(&router, None).await;
		assert_eq!(thread_count(&initial, &room, &first), Some(1), "initial sync");
		assert_eq!(thread_count(&initial, &room, &second), Some(1), "initial sync");

		// The reader opens the first thread; the second is still unread.
		let path = format!("/rooms/{room}/receipt/m.read/{first_reply}");
		request(&router, Method::POST, &path, READER_TOKEN, json!({"thread_id": first})).await;

		let read = sync(&router, Some(&initial)).await;
		assert!(joined(&read, &room).is_object(), "the receipt round carries the room: {read}");
		assert_eq!(thread_count(&read, &room, &first).unwrap_or(0), 0, "read thread: {read}");
		assert_eq!(thread_count(&read, &room, &second), Some(1), "unread thread: {read}");

		// Another member's receipt carries the room without moving the reader's
		// read positions.
		let path = format!("/rooms/{room}/receipt/m.read/{second}");
		request(&router, Method::POST, &path, HELPER_TOKEN, json!({})).await;

		let other = sync(&router, Some(&read)).await;
		assert!(joined(&other, &room).is_object(), "another receipt carries the room: {other}");
		assert_eq!(thread_count(&other, &room, &second), Some(1), "unread thread: {other}");

		// Activity elsewhere wakes the sync, but nothing changed in this room.
		let elsewhere = create_room(&router, HELPER_TOKEN).await;
		send(&router, &elsewhere, HELPER_TOKEN, "elsewhere", text("Meanwhile")).await;

		let quiet = sync(&router, Some(&other)).await;
		assert!(joined(&quiet, &room).is_null(), "a quiet room stays out: {quiet}");

		Ok(())
	})
}

fn joined<'a>(sync: &'a Value, room: &RoomId) -> &'a Value {
	&sync["rooms"]["join"][room.as_str()]
}

fn thread_count(sync: &Value, room: &RoomId, root: &OwnedEventId) -> Option<u64> {
	joined(sync, room)["unread_thread_notifications"][root.as_str()]["notification_count"]
		.as_u64()
}

fn text(body: &str) -> Value { json!({"msgtype": "m.text", "body": body}) }

fn thread_reply(root: &OwnedEventId) -> Value {
	json!({
		"msgtype": "m.text",
		"body": "An answer",
		"m.relates_to": {"rel_type": "m.thread", "event_id": root},
	})
}

async fn sync(router: &Router, since: Option<&Value>) -> Value {
	let filter: String = url::form_urlencoded::byte_serialize(
		br#"{"room":{"timeline":{"unread_thread_notifications":true}}}"#,
	)
	.collect();
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
