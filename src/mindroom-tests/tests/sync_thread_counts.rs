//! Every sync that carries a room lists all its threads with notifications.

#![cfg(test)]

mod support;

use axum::{Router, body::Body};
use serde_json::{Value, json};
use tower::ServiceExt;
use tuwunel_core::{
	Result,
	http::{Method, Request, StatusCode, header},
	ruma::{OwnedEventId, OwnedRoomId, RoomId, UserId, user_id},
};
use tuwunel_service::Services;

use self::support::{Harness, api_router};

const READER_TOKEN: &str = "thread-counts-reader-test-token-0123456789abcdef";
const HELPER_TOKEN: &str = "thread-counts-helper-test-token-0123456789abcdef";
const READER_OTHER_DEVICE_TOKEN: &str = "thread-counts-reader-other-device-token-0123456789";

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
/// members. A thread read down to zero is sent as zero in the round its read
/// moves and left out after that, an unthreaded receipt reads them all, and a
/// room with nothing new still stays out of the response. The reader's own
/// send reads only the thread it is in, or the main timeline; a state event
/// reads nothing.
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

		let token = (Some(READER_OTHER_DEVICE_TOKEN), None);
		services
			.users
			.create_device(user_id!("@reader:localhost"), None, token, None, None, None)
			.await?;

		let (state, _guard) = tuwunel_api::router::state::create(services.clone());
		let router = api_router(&services.server).with_state(state);

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
		// Consume that typing update, so the receipt alone carries the room next.
		let stopped = sync(&router, THREADED, Some(&typing)).await;

		// The reader opens the first thread. Its read position moved in this
		// round, so the map sends it as zero; the second is still unread.
		receipt(&router, &room, READER_TOKEN, &first_reply, json!({"thread_id": first})).await;
		let read = sync(&router, THREADED, Some(&stopped)).await;
		assert_eq!(counts(&read, &room, &first), Some((0, 0)), "read thread: {read}");
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

		check_own_send(&services, &router).await;

		Ok(())
	})
}

/// MSC3771: the reader's own event marks as read only the timeline it belongs
/// to, like a receipt there. A reply in one thread leaves the main timeline and
/// the other thread unread. A reaction to a thread root and a main-timeline
/// message read only the main timeline; a reaction to a thread reply, and an
/// edit or a redaction in a thread, read only that thread. A state event (a
/// topic, a thread tag, a profile change) reads nothing, and only the reader's
/// join read the whole room. The push badge follows the same counts, and the
/// reader's private read markers stay in each event's own timeline.
async fn check_own_send(services: &Services, router: &Router) {
	let reader = user_id!("@reader:localhost");
	let room = create_room(router, READER_TOKEN).await;
	let path = format!("/rooms/{room}/join");
	request(router, Method::POST, &path, HELPER_TOKEN, json!({})).await;

	let x = send(router, &room, HELPER_TOKEN, "own-x", text("Question X")).await;
	let y = send(router, &room, HELPER_TOKEN, "own-y", text("Question Y")).await;
	let x_reply = send(router, &room, HELPER_TOKEN, "own-x-reply", thread_reply(&x, &[])).await;
	send(router, &room, HELPER_TOKEN, "own-y-reply", thread_reply(&y, &[])).await;
	send(router, &room, HELPER_TOKEN, "own-main", text("Main timeline")).await;

	let before = sync(router, THREADED, None).await;
	assert_eq!(room_counts(&before, &room), (Some(3), Some(0)), "before: {before}");
	assert_eq!(counts(&before, &room, &x), Some((1, 0)), "before: {before}");
	assert_eq!(counts(&before, &room, &y), Some((1, 0)), "before: {before}");

	let in_y_event = send(router, &room, READER_TOKEN, "own-in-y", thread_reply(&y, &[])).await;
	let in_y = sync(router, THREADED, Some(&before)).await;
	assert_eq!(room_counts(&in_y, &room), (Some(3), Some(0)), "reply in Y: {in_y}");
	assert_eq!(counts(&in_y, &room, &x), Some((1, 0)), "reply in Y: {in_y}");
	assert_eq!(client_counts(&in_y, &room, &y), Some((0, 0)), "reply in Y: {in_y}");
	assert_eq!(badge(services, reader).await, 4, "reply in Y");

	// A profile change sends a member event to every joined room.
	let state_events = [
		(format!("/rooms/{room}/state/m.room.topic"), json!({"topic": "Questions"})),
		(
			format!("/rooms/{room}/state/com.mindroom.thread.tags/{x}"),
			json!({"tags": {"resolved": {}}}),
		),
		(
			"/profile/@reader:localhost/displayname".to_owned(),
			json!({"displayname": "Reader"}),
		),
	];
	let mut state = in_y;
	for (path, body) in state_events {
		request(router, Method::PUT, &path, READER_TOKEN, body).await;
		state = sync(router, THREADED, Some(&state)).await;
		// Nothing was reset, so the zero highlight count is left out.
		assert_eq!(room_counts(&state, &room), (Some(3), None), "{path}: {state}");
		assert_eq!(counts(&state, &room, &x), Some((1, 0)), "{path}: {state}");
		assert_eq!(badge(services, reader).await, 4, "{path}");
	}
	let member = joined(&state, &room)["timeline"]["events"]
		.as_array()
		.into_iter()
		.flatten()
		.any(|event| event["content"]["displayname"] == "Reader");
	assert!(member, "profile change: {state}");

	send(router, &room, HELPER_TOKEN, "own-main-2", text("Another one")).await;
	react(router, &room, &x, "own-root-reaction").await;
	let root = sync(router, THREADED, Some(&state)).await;
	assert_eq!(room_counts(&root, &room), (Some(0), Some(0)), "reaction to root: {root}");
	assert_eq!(counts(&root, &room, &x), Some((1, 0)), "reaction to root: {root}");
	assert_eq!(badge(services, reader).await, 1, "reaction to root");

	send(router, &room, HELPER_TOKEN, "own-y-2", thread_reply(&y, &[])).await;
	send(router, &room, HELPER_TOKEN, "own-main-3", text("And another")).await;
	let edit = send(router, &room, READER_TOKEN, "own-y-edit", edit_of(&in_y_event)).await;
	let edited = sync(router, THREADED, Some(&root)).await;
	assert_eq!(room_counts(&edited, &room), (Some(1), Some(0)), "edit in Y: {edited}");
	assert_eq!(counts(&edited, &room, &x), Some((1, 0)), "edit in Y: {edited}");
	assert_eq!(client_counts(&edited, &room, &y), Some((0, 0)), "edit in Y: {edited}");
	assert_eq!(badge(services, reader).await, 2, "edit in Y");

	send(router, &room, HELPER_TOKEN, "own-y-3", thread_reply(&y, &[])).await;
	let path = format!("/rooms/{room}/redact/{edit}/own-y-redact");
	let response = request(router, Method::PUT, &path, READER_TOKEN, json!({})).await;
	let redaction = response["event_id"]
		.as_str()
		.expect("redaction event id");
	let redacted = sync(router, THREADED, Some(&edited)).await;
	assert_eq!(room_counts(&redacted, &room), (Some(1), Some(0)), "redaction: {redacted}");
	assert_eq!(counts(&redacted, &room, &x), Some((1, 0)), "redaction: {redacted}");
	assert_eq!(client_counts(&redacted, &room, &y), Some((0, 0)), "redaction: {redacted}");
	assert_eq!(badge(services, reader).await, 2, "redaction");

	send(router, &room, HELPER_TOKEN, "own-main-4", text("And one more")).await;
	let in_main_event =
		send(router, &room, READER_TOKEN, "own-in-main", text("My question")).await;
	let in_main = sync(router, THREADED, Some(&redacted)).await;
	assert_eq!(room_counts(&in_main, &room), (Some(0), Some(0)), "main message: {in_main}");
	assert_eq!(counts(&in_main, &room, &x), Some((1, 0)), "main message: {in_main}");
	assert_eq!(badge(services, reader).await, 1, "main message");

	let reaction = react(router, &room, &x_reply, "own-reply-reaction").await;
	let reacted = sync(router, THREADED, Some(&in_main)).await;
	assert_eq!(client_counts(&reacted, &room, &x), Some((0, 0)), "reaction in X: {reacted}");
	assert_eq!(badge(services, reader).await, 0, "reaction in X");

	let (unthreaded, threaded): (Vec<_>, Vec<_>) = private_markers(services, &room, reader)
		.await
		.into_iter()
		.partition(|(_, thread)| thread.is_none());

	let mut expected = vec![
		(redaction.to_owned(), Some(y.to_string())),
		(in_main_event.to_string(), Some("main".to_owned())),
		(reaction.to_string(), Some(x.to_string())),
	];
	expected.sort();
	assert_eq!(threaded, expected);

	// The one unthreaded marker is the reader's join.
	let [(join_event, None)] = unthreaded.as_slice() else {
		panic!("one unthreaded marker: {unthreaded:?}");
	};
	let path = format!("/rooms/{room}/event/{join_event}");
	let join = request(router, Method::GET, &path, READER_TOKEN, Value::Null).await;
	assert_eq!(join["type"], "m.room.member", "{join}");
	assert_eq!(join["content"]["membership"], "join", "{join}");

	check_receipt_after_own_send(services, router, &room, &y).await;
}

/// The server marks the reader's own event read without telling their other
/// devices, so an explicit private receipt for it, in its thread or in the main
/// timeline, still reaches them, and resets nothing that came after it.
/// Repeating the receipt sends nothing new.
async fn check_receipt_after_own_send(
	services: &Services,
	router: &Router,
	room: &RoomId,
	y: &OwnedEventId,
) {
	let reader = user_id!("@reader:localhost");
	let mut other = sync_as(router, READER_OTHER_DEVICE_TOKEN, THREADED, None).await;

	for (txn, content, thread) in [
		("receipt-in-y", thread_reply(y, &[]), y.as_str()),
		("receipt-in-main", text("Once more"), "main"),
	] {
		let event = send(router, room, READER_TOKEN, txn, content.clone()).await;
		let after = format!("{txn}-after");
		send(router, room, HELPER_TOKEN, &after, content).await;
		other = sync_as(router, READER_OTHER_DEVICE_TOKEN, THREADED, Some(&other)).await;

		let unread = badge(services, reader).await;
		let path = format!("/rooms/{room}/receipt/m.read.private/{event}");
		let body = json!({"thread_id": thread});
		request(router, Method::POST, &path, READER_TOKEN, body.clone()).await;
		other = sync_as(router, READER_OTHER_DEVICE_TOKEN, THREADED, Some(&other)).await;

		let marker = (event.to_string(), Some(thread.to_owned()));
		let receipts = private_receipts(&other, room);
		assert!(receipts.contains(&marker), "{thread} receipt: {other}");
		assert_eq!(badge(services, reader).await, unread, "{thread} receipt reset a count");

		request(router, Method::POST, &path, READER_TOKEN, body).await;
		other = sync_as(router, READER_OTHER_DEVICE_TOKEN, THREADED, Some(&other)).await;
		assert!(private_receipts(&other, room).is_empty(), "{thread} replay: {other}");
	}
}

/// The `(event, thread_id)` of each private read receipt the reader's devices
/// receive for the room in this sync.
fn private_receipts(sync: &Value, room: &RoomId) -> Vec<(String, Option<String>)> {
	joined(sync, room)["ephemeral"]["events"]
		.as_array()
		.into_iter()
		.flatten()
		.filter(|event| event["type"] == "m.receipt")
		.filter_map(|event| event["content"].as_object())
		.flatten()
		.filter_map(|(id, receipts)| {
			let receipt = receipts["m.read.private"]["@reader:localhost"].as_object()?;
			let thread = receipt
				.get("thread_id")
				.and_then(Value::as_str)
				.map(ToOwned::to_owned);

			Some((id.clone(), thread))
		})
		.collect()
}

/// An edit of `event` that replaces its text.
fn edit_of(event: &OwnedEventId) -> Value {
	json!({
		"msgtype": "m.text",
		"body": "* An edited answer",
		"m.new_content": {"msgtype": "m.text", "body": "An edited answer"},
		"m.relates_to": {"rel_type": "m.replace", "event_id": event},
	})
}

/// The reader reacts to `event`.
async fn react(router: &Router, room: &RoomId, event: &OwnedEventId, txn: &str) -> OwnedEventId {
	let path = format!("/rooms/{room}/send/m.reaction/{txn}");
	let content = json!({
		"m.relates_to": {"rel_type": "m.annotation", "event_id": event, "key": "👍"},
	});
	let response = request(router, Method::PUT, &path, READER_TOKEN, content).await;

	response["event_id"]
		.as_str()
		.expect("reaction event id")
		.try_into()
		.expect("valid event id")
}

/// The `(event, thread_id)` of each private read marker the user holds in the
/// room, as their devices receive them; an unthreaded one has no `thread_id`.
async fn private_markers(
	services: &Services,
	room: &RoomId,
	user: &UserId,
) -> Vec<(String, Option<String>)> {
	let events = services
		.read_receipt
		.private_read_get(room, user)
		.await
		.expect("private read markers");

	let mut markers: Vec<_> = events
		.iter()
		.flat_map(|event| {
			let event: Value = serde_json::from_str(event.json().get()).expect("receipt JSON");
			let content = event["content"]
				.as_object()
				.expect("receipt content")
				.clone();

			content.into_iter().map(|(id, receipts)| {
				let thread = receipts["m.read.private"][user.as_str()]["thread_id"]
					.as_str()
					.map(ToOwned::to_owned);

				(id, thread)
			})
		})
		.collect();

	markers.sort();
	markers
}

/// The account-wide unread count a push carries as `counts.unread`.
async fn badge(services: &Services, user: &UserId) -> u64 {
	services
		.pusher
		.global_notification_count(user)
		.await
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
	sync_as(router, READER_TOKEN, filter, since).await
}

async fn sync_as(router: &Router, token: &str, filter: &str, since: Option<&Value>) -> Value {
	let filter: String = url::form_urlencoded::byte_serialize(filter.as_bytes()).collect();
	let since = since.map_or_else(String::new, |sync| {
		format!("&since={}", sync["next_batch"].as_str().expect("next_batch"))
	});

	request(
		router,
		Method::GET,
		&format!("/sync?timeout=0&filter={filter}{since}"),
		token,
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
