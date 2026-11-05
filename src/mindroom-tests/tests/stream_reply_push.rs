//! Terminal stream replies must use the same relations for counts and delivery,
//! and a finished stream counts like a new message.

#![cfg(test)]

mod support;

use std::time::Duration;

use axum::{Json, Router, body::Body, routing::post};
use serde_json::{Value, json};
use tokio::{
	sync::mpsc::{UnboundedReceiver, unbounded_channel},
	time::timeout,
};
use tower::ServiceExt;
use tuwunel_core::{
	Event, Result,
	http::{Request, StatusCode, header},
	ruma::{
		EventId, OwnedEventId, OwnedRoomId, RoomId, UserId,
		api::client::push::{Pusher, PusherIds, PusherInit, PusherKind},
		events::StateEventType,
		push::{HttpPusherData, Ruleset},
		user_id,
	},
};
use tuwunel_service::Services;

use self::support::Harness;

const AUTHOR_TOKEN: &str = "stream-reply-author-test-token-0123456789abcdef";
const HELPER_TOKEN: &str = "stream-reply-helper-test-token-0123456789abcdef";
const OBSERVER_TOKEN: &str = "stream-reply-observer-test-token-0123456789abcdef";

const MESSAGE: &str = "m.room.message";
const ENCRYPTED: &str = "m.room.encrypted";

#[test]
fn terminal_replies_preserve_highlight_and_gateway_tweaks() -> Result {
	let harness = Harness::new("mindroom_stream_reply_push", [
		"msc3664_related_event_match=true".into(),
		"ip_range_denylist=[]".into(),
	])?;
	let (tx, mut captured) = unbounded_channel();
	let gateway = harness.mock_server(Router::new().route(
		"/_matrix/push/v1/notify",
		post(async move |Json(body): Json<Value>| {
			tx.send(body).expect("capture push notification");
			Json(json!({"rejected": []}))
		}),
	))?;
	let url = format!("{}/_matrix/push/v1/notify", gateway.base_url);
	let pusher = PusherInit {
		ids: PusherIds::new("stream-reply-pushkey".into(), "app.stream-reply.test".into()),
		kind: PusherKind::Http(HttpPusherData::new(url.clone())),
		app_display_name: "Stream reply test".into(),
		device_display_name: "Test device".into(),
		profile_tag: None,
		lang: "en".into(),
	}
	.into();

	let result = harness.with_services(async |services| {
		let author = user_id!("@author:localhost");
		for (user, token) in [
			(author, AUTHOR_TOKEN),
			(user_id!("@helper:localhost"), HELPER_TOKEN),
			(user_id!("@observer:localhost"), OBSERVER_TOKEN),
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
		for token in [HELPER_TOKEN, OBSERVER_TOKEN] {
			request(&router, "POST", &format!("/rooms/{room}/join"), token, json!({})).await;
		}

		exercise_replies(&services, &router, author, &room, &pusher, &mut captured).await?;
		check_invite_target(&services, &router, &pusher, &mut captured).await?;
		check_finished_replies(&services, &router, &url, &mut captured).await
	});
	gateway.handle.abort();
	result
}

async fn exercise_replies(
	services: &Services,
	router: &Router,
	author: &UserId,
	room: &RoomId,
	pusher: &Pusher,
	captured: &mut UnboundedReceiver<Value>,
) -> Result {
	let question = send(
		router,
		room,
		AUTHOR_TOKEN,
		"question",
		json!({"msgtype": "m.text", "body": "What is the answer?"}),
	)
	.await;
	let stream = send(
		router,
		room,
		HELPER_TOKEN,
		"stream",
		json!({
			"msgtype": "m.text",
			"body": "Thinking",
			"io.mindroom.stream_status": "streaming",
		}),
	)
	.await;

	for status in ["completed", "cancelled", "interrupted", "error"] {
		let event =
			send(router, room, HELPER_TOKEN, status, terminal_reply(status, &stream, &question))
				.await;
		check_reply(services, author, &event, pusher, captured, true).await?;
	}

	// A nested reply must retain the resolver's same-room visibility boundary.
	let elsewhere = create_room(router).await;
	let hidden = send(
		router,
		&elsewhere,
		AUTHOR_TOKEN,
		"hidden",
		json!({"msgtype": "m.text", "body": "Not visible in the stream room"}),
	)
	.await;
	let event = send(
		router,
		room,
		HELPER_TOKEN,
		"cross-room",
		terminal_reply("completed", &stream, &hidden),
	)
	.await;
	check_reply(services, author, &event, pusher, captured, false).await?;

	// The client send awaits append/count evaluation before returning.
	assert_eq!(
		services
			.pusher
			.highlight_count(author, room)
			.await,
		4
	);
	assert_eq!(
		services
			.pusher
			.notification_count(author, room)
			.await,
		5
	);

	Ok(())
}

/// A finished stream counts like a new message, because a reply read half-way
/// has not been read: its terminal edit adds the unread count its final content
/// earns under the author's push rules and delivers one push, even after a
/// receipt or the author's own send while it streamed. The next receipt clears
/// it. The stream's earlier events count and push nothing, which the gateway
/// shows by receiving each expected push next.
async fn check_finished_replies(
	services: &Services,
	router: &Router,
	gateway: &str,
	captured: &mut UnboundedReceiver<Value>,
) -> Result {
	let pusher = json!({
		"pushkey": "stream-reply-device",
		"kind": "http",
		"app_id": "app.stream-reply.test",
		"app_display_name": "Stream reply test",
		"device_display_name": "Test device",
		"lang": "en",
		"data": {"url": gateway},
	});
	request(router, "POST", "/pushers/set", AUTHOR_TOKEN, pusher).await;

	let room = shared_room(router).await;
	let question = send(router, &room, AUTHOR_TOKEN, "finished-question", text("Why?")).await;
	check_read_while_streaming(services, router, captured, &room, &question).await;
	check_mentions_only(services, router, captured).await;
	check_encrypted(services, router, captured).await;

	// Nothing else is queued: the next push is the next message's.
	let last = send(router, &room, HELPER_TOKEN, "last", text("Anything else?")).await;
	expect_push(services, captured, &last, false).await;

	Ok(())
}

/// Receipts the author sent while the reply streamed (in its thread,
/// unthreaded or `"main"`) and the author's own sends meanwhile, such as the
/// reaction that stops it, leave the finished reply counted.
async fn check_read_while_streaming(
	services: &Services,
	router: &Router,
	captured: &mut UnboundedReceiver<Value>,
	room: &RoomId,
	question: &EventId,
) {
	let thread = Some(question.as_str());

	let read = Reply::new(room, Some(question), MESSAGE, reply_to(question), "read");
	let (first, update) = read.start(services, router).await;
	receipt(router, room, &first, thread).await;
	let path = format!("/rooms/{room}/read_markers");
	request(router, "POST", &path, AUTHOR_TOKEN, json!({"m.read.private": update})).await;
	let done = read
		.finish(services, router, captured, &first, "completed", (1, 1))
		.await;
	// Only an advancing receipt resets: the first event again leaves the count.
	receipt(router, room, &first, thread).await;
	assert_eq!(read.counts(services).await, (1, 1));
	receipt(router, room, &done, thread).await;
	assert_eq!(read.counts(services).await, (0, 0));

	let stopped = Reply::new(room, Some(question), MESSAGE, reply_to(question), "stopped");
	let (first, _) = stopped.start(services, router).await;
	let stop =
		json!({"m.relates_to": {"rel_type": "m.annotation", "event_id": first, "key": "🛑"}});
	send_event(router, room, AUTHOR_TOKEN, "m.reaction", "stop", stop).await;
	let done = stopped
		.finish(services, router, captured, &first, "cancelled", (1, 1))
		.await;
	receipt(router, room, &done, thread).await;
	assert_eq!(stopped.counts(services).await, (0, 0));

	let main = Reply::new(room, None, MESSAGE, reply_to(question), "main");
	let (first, _) = main.start(services, router).await;
	receipt(router, room, &first, Some("main")).await;
	send(router, room, AUTHOR_TOKEN, "meanwhile", text("Meanwhile, another question")).await;
	let done = main
		.finish(services, router, captured, &first, "completed", (1, 1))
		.await;
	receipt(router, room, &done, Some("main")).await;
	assert_eq!(main.counts(services).await, (0, 0));
}

/// In a room the author set to mentions only, a finished reply counts and
/// pushes only when its final text mentions them, even when nothing before it
/// did.
async fn check_mentions_only(
	services: &Services,
	router: &Router,
	captured: &mut UnboundedReceiver<Value>,
) {
	let room = shared_room(router).await;
	// Loading the rules, as clients do first, stores the defaults this account
	// was created without.
	request(router, "GET", "/pushrules/global/", AUTHOR_TOKEN, json!({})).await;
	let path = format!("/pushrules/global/room/{room}");
	request(router, "PUT", &path, AUTHOR_TOKEN, json!({"actions": []})).await;
	let question = send(router, &room, AUTHOR_TOKEN, "quiet-question", text("Anyone?")).await;

	// Unmentioned, it counts nothing and pushes nothing: the next push is the
	// mention's.
	let quiet = Reply::new(&room, None, MESSAGE, text("Final answer"), "quiet");
	let (first, _) = quiet.start(services, router).await;
	quiet
		.finish(services, router, captured, &first, "completed", (0, 0))
		.await;

	let mentioned = json!({
		"msgtype": "m.text",
		"body": "Final answer for author",
		"m.mentions": {"user_ids": ["@author:localhost"]},
	});
	let mention = Reply::new(&room, Some(&question), MESSAGE, mentioned, "mention");
	let (first, _) = mention.start(services, router).await;
	let done = mention
		.finish(services, router, captured, &first, "completed", (1, 1))
		.await;
	receipt(router, &room, &done, Some(question.as_str())).await;
	assert_eq!(mention.counts(services).await, (0, 0));
}

/// Encrypted streams carry their status and relation in cleartext. The finished
/// reply counts by the encrypted message rules, which notify without a
/// highlight, after a receipt or an own send while it streamed.
async fn check_encrypted(
	services: &Services,
	router: &Router,
	captured: &mut UnboundedReceiver<Value>,
) {
	let room = shared_room(router).await;
	let path = format!("/rooms/{room}/state/m.room.encryption");
	request(router, "PUT", &path, AUTHOR_TOKEN, json!({"algorithm": "m.megolm.v1.aes-sha2"}))
		.await;
	let question =
		send_event(router, &room, AUTHOR_TOKEN, ENCRYPTED, "secret", encrypted()).await;

	let threaded = Reply::new(&room, Some(&question), ENCRYPTED, Value::Null, "secret-thread");
	let (first, _) = threaded.start(services, router).await;
	receipt(router, &room, &first, Some(question.as_str())).await;
	let done = threaded
		.finish(services, router, captured, &first, "completed", (1, 0))
		.await;
	receipt(router, &room, &done, None).await;
	assert_eq!(threaded.counts(services).await, (0, 0));

	let main = Reply::new(&room, None, ENCRYPTED, Value::Null, "secret-main");
	let (first, _) = main.start(services, router).await;
	send_event(router, &room, AUTHOR_TOKEN, ENCRYPTED, "secret-meanwhile", encrypted()).await;
	let done = main
		.finish(services, router, captured, &first, "completed", (1, 0))
		.await;
	receipt(router, &room, &done, Some("main")).await;
	assert_eq!(main.counts(services).await, (0, 0));
}

/// A reply the helper streams to the author: a first event and a non-terminal
/// update, then a terminal edit whose final message is `content`.
struct Reply<'a> {
	room: &'a RoomId,
	/// The thread it streams in, or `None` for the main timeline.
	thread: Option<&'a EventId>,
	/// `m.room.message`, or `m.room.encrypted`, whose events keep the stream
	/// status and relation in cleartext beside an opaque payload.
	kind: &'a str,
	content: Value,
	/// Prefixes the transaction ids of its events.
	label: &'a str,
}

impl<'a> Reply<'a> {
	fn new(
		room: &'a RoomId,
		thread: Option<&'a EventId>,
		kind: &'a str,
		content: Value,
		label: &'a str,
	) -> Self {
		Self { room, thread, kind, content, label }
	}

	/// Sends the first event and a non-terminal update, which count nothing;
	/// returns both.
	async fn start(&self, services: &Services, router: &Router) -> (OwnedEventId, OwnedEventId) {
		let mut first = self.payload(text("Thinking"));
		first["io.mindroom.stream_status"] = "streaming".into();
		if let Some(root) = self.thread {
			first["m.relates_to"] = json!({"rel_type": "m.thread", "event_id": root});
		}

		let first = self.send(router, "first", first).await;
		let update = self
			.edit(router, &first, "streaming", &text("Thinking about it"))
			.await;

		assert_eq!(
			self.counts(services).await,
			(0, 0),
			"{}: counted while streaming",
			self.label
		);
		(first, update)
	}

	/// Sends the terminal edit of `first`. Returns it once it counts as
	/// `expected` and, when it notifies, its push is the next the gateway
	/// receives.
	async fn finish(
		&self,
		services: &Services,
		router: &Router,
		captured: &mut UnboundedReceiver<Value>,
		first: &EventId,
		status: &str,
		expected: (u64, u64),
	) -> OwnedEventId {
		let done = self
			.edit(router, first, status, &self.content)
			.await;

		assert_eq!(self.counts(services).await, expected, "{}: finished", self.label);
		if expected.0 > 0 {
			expect_push(services, captured, &done, expected.1 > 0).await;
		}

		done
	}

	async fn edit(
		&self,
		router: &Router,
		first: &EventId,
		status: &str,
		new_content: &Value,
	) -> OwnedEventId {
		let mut edit = self.payload(json!({
			"msgtype": "m.text",
			"body": "* Final answer",
			"m.new_content": new_content,
		}));
		edit["io.mindroom.stream_status"] = status.into();
		edit["m.relates_to"] = json!({"rel_type": "m.replace", "event_id": first});

		self.send(router, status, edit).await
	}

	/// `message` itself, or an opaque payload in its place when encrypted.
	fn payload(&self, message: Value) -> Value {
		if self.kind == ENCRYPTED { encrypted() } else { message }
	}

	async fn send(&self, router: &Router, step: &str, content: Value) -> OwnedEventId {
		let txn = format!("{}-{step}", self.label);
		send_event(router, self.room, HELPER_TOKEN, self.kind, &txn, content).await
	}

	/// The author's `(notification, highlight)` counts where it counts: its
	/// thread, or the room's main timeline.
	async fn counts(&self, services: &Services) -> (u64, u64) {
		let author = user_id!("@author:localhost");
		match self.thread {
			| Some(root) => services
				.pusher
				.thread_notification_counts(author, self.room)
				.await
				.get(root)
				.copied()
				.unwrap_or_default(),
			| None => (
				services
					.pusher
					.notification_count(author, self.room)
					.await,
				services
					.pusher
					.highlight_count(author, self.room)
					.await,
			),
		}
	}
}

/// Waits for the gateway's next push for an event, skipping the counts-only
/// badge updates that receipts and own sends trigger, and checks that it is
/// `event`'s and carries the author's unread total.
async fn expect_push(
	services: &Services,
	captured: &mut UnboundedReceiver<Value>,
	event: &EventId,
	highlight: bool,
) {
	let notification = loop {
		let body = timeout(Duration::from_secs(10), captured.recv())
			.await
			.expect("gateway must receive the push")
			.expect("gateway capture must stay open");

		if body["notification"].get("event_id").is_some() {
			break body["notification"].clone();
		}
	};

	assert_eq!(notification["event_id"], event.as_str());
	let tweaks = &notification["devices"][0]["tweaks"];
	assert_eq!(tweaks["highlight"].as_bool().unwrap_or(false), highlight, "{notification}");
	let unread = services
		.pusher
		.global_notification_count(user_id!("@author:localhost"))
		.await;
	assert_eq!(notification["counts"]["unread"], unread);
}

/// The author's `m.read` receipt on `event`, in `thread` (a root or `"main"`)
/// or unthreaded.
async fn receipt(router: &Router, room: &RoomId, event: &EventId, thread: Option<&str>) {
	let path = format!("/rooms/{room}/receipt/m.read/{event}");
	let body = thread.map_or_else(|| json!({}), |thread| json!({"thread_id": thread}));
	request(router, "POST", &path, AUTHOR_TOKEN, body).await;
}

/// A room of the author and the helper.
async fn shared_room(router: &Router) -> OwnedRoomId {
	let room = create_room(router).await;
	request(router, "POST", &format!("/rooms/{room}/join"), HELPER_TOKEN, json!({})).await;
	room
}

fn text(body: &str) -> Value { json!({"msgtype": "m.text", "body": body}) }

fn reply_to(target: &EventId) -> Value {
	json!({
		"msgtype": "m.text",
		"body": "Final answer",
		"m.relates_to": {"m.in_reply_to": {"event_id": target}},
	})
}

fn encrypted() -> Value {
	json!({
		"algorithm": "m.megolm.v1.aes-sha2",
		"ciphertext": "opaque",
		"device_id": "DEVICE",
		"sender_key": "sender-key",
		"session_id": "session",
	})
}

/// Stream normalization must preserve upstream's recipient-based invite target.
async fn check_invite_target(
	services: &Services,
	router: &Router,
	pusher: &Pusher,
	captured: &mut UnboundedReceiver<Value>,
) -> Result {
	let invitee = user_id!("@observer:localhost");
	let room = create_room(router).await;
	request(
		router,
		"POST",
		&format!("/rooms/{room}/invite"),
		AUTHOR_TOKEN,
		json!({"user_id": invitee}),
	)
	.await;
	let event = services
		.state_accessor
		.room_state_get(&room, &StateEventType::RoomMember, invitee.as_str())
		.await?;
	assert_eq!(event.sender(), user_id!("@author:localhost"));
	assert_eq!(event.state_key(), Some(invitee.as_str()));
	services
		.pusher
		.send_push_notice(invitee, pusher, &Ruleset::server_default(invitee), &event)
		.await?;
	let body = timeout(Duration::from_secs(5), captured.recv())
		.await
		.expect("gateway must receive the invite notification")
		.expect("gateway capture must stay open");
	let notification = &body["notification"];
	assert_eq!(notification["event_id"], event.event_id().as_str());
	assert_eq!(notification["type"], "m.room.member");
	assert_eq!(notification["sender"], "@author:localhost");
	assert_eq!(notification["content"]["membership"], "invite");
	assert_eq!(notification["user_is_target"], true);
	Ok(())
}

async fn check_reply(
	services: &Services,
	author: &UserId,
	event: &EventId,
	pusher: &Pusher,
	captured: &mut UnboundedReceiver<Value>,
	highlight: bool,
) -> Result {
	let pdu = services.timeline.get_pdu(event).await?;
	services
		.pusher
		.send_push_notice(author, pusher, &Ruleset::server_default(author), &pdu)
		.await?;
	let body = timeout(Duration::from_secs(5), captured.recv())
		.await
		.expect("gateway must receive the terminal notification")
		.expect("gateway capture must stay open");
	let notification = &body["notification"];
	assert_eq!(notification["event_id"], event.as_str());
	assert_eq!(notification["content"]["body"], "Final answer");
	assert!(
		notification["content"]
			.get("m.new_content")
			.is_none()
	);

	let tweaks = &notification["devices"][0]["tweaks"];
	if highlight {
		// High is the omitted default in the Push Gateway API.
		assert!(
			notification
				.get("prio")
				.is_none_or(|priority| priority == "high"),
			"terminal reply must have high priority: {notification}",
		);
		assert_eq!(tweaks["highlight"], true);
		assert_eq!(tweaks["sound"], "default");
	} else {
		assert_eq!(notification["prio"], "low");
		assert!(tweaks.get("highlight").is_none());
		assert!(tweaks.get("sound").is_none());
	}

	Ok(())
}

fn terminal_reply(status: &str, stream: &EventId, target: &EventId) -> Value {
	json!({
		"msgtype": "m.text",
		"body": "* Final answer",
		"io.mindroom.stream_status": status,
		"m.relates_to": {"rel_type": "m.replace", "event_id": stream},
		"m.new_content": reply_to(target),
	})
}

async fn create_room(router: &Router) -> OwnedRoomId {
	let response =
		request(router, "POST", "/createRoom", AUTHOR_TOKEN, json!({"preset": "public_chat"}))
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
	send_event(router, room, token, "m.room.message", txn, content).await
}

async fn send_event(
	router: &Router,
	room: &RoomId,
	token: &str,
	event_type: &str,
	txn: &str,
	content: Value,
) -> OwnedEventId {
	let response =
		request(router, "PUT", &format!("/rooms/{room}/send/{event_type}/{txn}"), token, content)
			.await;
	response["event_id"]
		.as_str()
		.expect("sent event id")
		.try_into()
		.expect("valid event id")
}

async fn request(router: &Router, method: &str, path: &str, token: &str, body: Value) -> Value {
	let request = Request::builder()
		.method(method)
		.uri(format!("/_matrix/client/v3{path}"))
		.header(header::AUTHORIZATION, format!("Bearer {token}"))
		.header(header::CONTENT_TYPE, "application/json")
		.header("X-Forwarded-For", "127.0.0.1")
		.body(Body::from(body.to_string()))
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
