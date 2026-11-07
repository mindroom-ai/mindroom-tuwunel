//! `/notifications` marks a thread reply read only by a read that covers its
//! thread.

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

const READER_TOKEN: &str = "notifications-thread-reader-token-0123456789abcdef";
const HELPER_TOKEN: &str = "notifications-thread-helper-token-0123456789abcdef";

/// MSC3771: a `"main"` receipt, or the reader's own main-timeline message,
/// reads only the main timeline, so the room's thread replies stay unread in
/// `/notifications` as they do in the sync counts. An unthreaded receipt reads
/// every thread through its event, and a later reply in that thread leaves the
/// earlier replies read. A receipt in the thread reads its replies.
#[test]
fn main_read_leaves_thread_replies_unread() -> Result {
	let harness = Harness::new("mindroom_notifications_thread_read", [])?;

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

		let room = create_room(&router).await;
		let path = format!("/rooms/{room}/join");
		request(&router, Method::POST, &path, HELPER_TOKEN, json!({})).await;

		let root = send(&router, &room, HELPER_TOKEN, "root", text("Question")).await;
		let reply = send(&router, &room, HELPER_TOKEN, "reply", thread_reply(&root)).await;
		let main = send(&router, &room, HELPER_TOKEN, "main", text("Main timeline")).await;

		receipt(&router, &room, &main, json!({"thread_id": "main"})).await;
		let flags = read_flags(&router).await;
		assert_eq!(flag(&flags, &root), Some(true), "main receipt: {flags:?}");
		assert_eq!(flag(&flags, &main), Some(true), "main receipt: {flags:?}");
		assert_eq!(flag(&flags, &reply), Some(false), "main receipt: {flags:?}");

		send(&router, &room, READER_TOKEN, "own", text("Own message")).await;
		let flags = read_flags(&router).await;
		assert_eq!(flag(&flags, &reply), Some(false), "own main message: {flags:?}");

		receipt(&router, &room, &main, json!({})).await;
		let flags = read_flags(&router).await;
		assert_eq!(flag(&flags, &reply), Some(true), "unthreaded receipt: {flags:?}");

		let again = send(&router, &room, HELPER_TOKEN, "again", thread_reply(&root)).await;
		let later = send(&router, &room, HELPER_TOKEN, "later", text("Later")).await;
		receipt(&router, &room, &later, json!({"thread_id": "main"})).await;
		let flags = read_flags(&router).await;
		assert_eq!(flag(&flags, &reply), Some(true), "earlier reply: {flags:?}");
		assert_eq!(flag(&flags, &later), Some(true), "later main: {flags:?}");
		assert_eq!(flag(&flags, &again), Some(false), "later reply: {flags:?}");

		receipt(&router, &room, &again, json!({"thread_id": root})).await;
		let flags = read_flags(&router).await;
		assert_eq!(flag(&flags, &again), Some(true), "thread receipt: {flags:?}");

		Ok(())
	})
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

async fn receipt(router: &Router, room: &RoomId, event: &OwnedEventId, body: Value) {
	let path = format!("/rooms/{room}/receipt/m.read/{event}");
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
