//! Terminal stream replies must use the same relations for counts and delivery.

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
	let pusher = PusherInit {
		ids: PusherIds::new("stream-reply-pushkey".into(), "app.stream-reply.test".into()),
		kind: PusherKind::Http(HttpPusherData::new(format!(
			"{}/_matrix/push/v1/notify",
			gateway.base_url,
		))),
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
		check_read_reply(&services, &router).await?;
		check_unthreaded_read(&services, &router).await?;
		check_other_receipts(&services, &router).await?;
		check_private_behind_own_send(&services, &router).await?;
		check_private_after_completion(&services, &router).await?;
		check_thread_read_kept(&services, &router).await?;
		check_shared_readers(&services, &router).await
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

/// A terminal edit of a reply the user already read must not count as unread:
/// clients never send a receipt for it, so the count and the push badge would
/// otherwise stay up with nothing left to read.
async fn check_read_reply(services: &Services, router: &Router) -> Result {
	let author = user_id!("@author:localhost");
	let room = create_room(router).await;
	request(router, "POST", &format!("/rooms/{room}/join"), HELPER_TOKEN, json!({})).await;
	let question = send(
		router,
		&room,
		AUTHOR_TOKEN,
		"read-question",
		json!({"msgtype": "m.text", "body": "What is the answer?"}),
	)
	.await;
	let before = services
		.pusher
		.global_notification_count(author)
		.await;

	check_watched_thread(services, router, &room, &question).await;
	assert_eq!(
		services
			.pusher
			.global_notification_count(author)
			.await,
		before
	);

	check_unread_thread(services, router, &room, &question).await?;
	assert_eq!(
		services
			.pusher
			.global_notification_count(author)
			.await,
		before.saturating_add(1)
	);

	// A main-timeline reply read mid-stream stays read as well.
	let main = send(
		router,
		&room,
		HELPER_TOKEN,
		"main",
		json!({"msgtype": "m.text", "body": "Thinking", "io.mindroom.stream_status": "streaming"}),
	)
	.await;
	receipt(router, &room, &main, "main").await;
	send(
		router,
		&room,
		HELPER_TOKEN,
		"main-done",
		terminal_reply("completed", &main, &question),
	)
	.await;
	assert_eq!(
		(
			services
				.pusher
				.notification_count(author, &room)
				.await,
			services
				.pusher
				.highlight_count(author, &room)
				.await,
		),
		(0, 0)
	);

	Ok(())
}

/// Thread replies read mid-stream stay read when they finish, including one
/// the user stops and an encrypted one.
async fn check_watched_thread(
	services: &Services,
	router: &Router,
	room: &RoomId,
	question: &EventId,
) {
	let thread = json!({"rel_type": "m.thread", "event_id": question});

	let read = send(router, room, HELPER_TOKEN, "read", stream_reply(&thread)).await;
	receipt(router, room, &read, question.as_str()).await;
	send(
		router,
		room,
		HELPER_TOKEN,
		"read-done",
		terminal_reply("completed", &read, question),
	)
	.await;
	assert_eq!(thread_counts(services, room, question).await, (0, 0));

	// Stopping a watched stream is the user's own send (a reaction): it clears
	// the thread watermark with the counts, and the cancelled reply stays read.
	let stopped = send(router, room, HELPER_TOKEN, "stopped", stream_reply(&thread)).await;
	receipt(router, room, &stopped, question.as_str()).await;
	send_event(
		router,
		room,
		AUTHOR_TOKEN,
		"m.reaction",
		"stop",
		json!({"m.relates_to": {"rel_type": "m.annotation", "event_id": stopped, "key": "🛑"}}),
	)
	.await;
	send(
		router,
		room,
		HELPER_TOKEN,
		"stopped-done",
		terminal_reply("cancelled", &stopped, question),
	)
	.await;
	assert_eq!(thread_counts(services, room, question).await, (0, 0));

	// Encrypted streams carry the stream status and relation in cleartext.
	let encrypted = send_event(
		router,
		room,
		HELPER_TOKEN,
		"m.room.encrypted",
		"encrypted",
		encrypted_stream("streaming", &thread),
	)
	.await;
	receipt(router, room, &encrypted, question.as_str()).await;
	send_event(
		router,
		room,
		HELPER_TOKEN,
		"m.room.encrypted",
		"encrypted-done",
		encrypted_stream("completed", &json!({"rel_type": "m.replace", "event_id": encrypted})),
	)
	.await;
	assert_eq!(thread_counts(services, room, question).await, (0, 0));
}

/// A main-timeline receipt moves the room watermark but resets no thread, and
/// the user's own send elsewhere in the room says nothing about this thread,
/// so the finished reply of an unread thread still counts.
async fn check_unread_thread(
	services: &Services,
	router: &Router,
	room: &RoomId,
	question: &EventId,
) -> Result {
	let thread = json!({"rel_type": "m.thread", "event_id": question});
	let unread = send(router, room, HELPER_TOKEN, "unread", stream_reply(&thread)).await;
	receipt(router, room, question, "main").await;
	let unread_count = services
		.timeline
		.get_pdu_count(&unread)
		.await?
		.into_unsigned();
	assert!(
		services
			.pusher
			.last_notification_read(user_id!("@author:localhost"), room)
			.await?
			> unread_count
	);
	send(
		router,
		room,
		AUTHOR_TOKEN,
		"elsewhere",
		json!({"msgtype": "m.text", "body": "Meanwhile, another question"}),
	)
	.await;
	send(
		router,
		room,
		HELPER_TOKEN,
		"unread-done",
		terminal_reply("completed", &unread, question),
	)
	.await;
	assert_eq!(thread_counts(services, room, question).await, (1, 1));

	Ok(())
}

/// MSC3771: an unthreaded receipt, public or private, marks every thread read
/// up to its event. It clears the thread counts, so a reply it covers stays
/// read when its stream finishes, while a reply that started after its event
/// still counts.
async fn check_unthreaded_read(services: &Services, router: &Router) -> Result {
	let (room, question) = thread_room(router, "unthreaded").await;
	let thread = json!({"rel_type": "m.thread", "event_id": question});

	// A receipt on a later event in another thread covers this thread too.
	let public = send(router, &room, HELPER_TOKEN, "public", stream_reply(&thread)).await;
	let other = send(
		router,
		&room,
		HELPER_TOKEN,
		"other",
		json!({"msgtype": "m.text", "body": "Another question"}),
	)
	.await;
	let other_reply = send(
		router,
		&room,
		HELPER_TOKEN,
		"other-reply",
		json!({
			"msgtype": "m.text",
			"body": "Another answer",
			"m.relates_to": {"rel_type": "m.thread", "event_id": other},
		}),
	)
	.await;
	request(
		router,
		"POST",
		&format!("/rooms/{room}/receipt/m.read/{other_reply}"),
		AUTHOR_TOKEN,
		json!({}),
	)
	.await;
	send(
		router,
		&room,
		HELPER_TOKEN,
		"public-done",
		terminal_reply("completed", &public, &question),
	)
	.await;
	assert_eq!(thread_counts(services, &room, &question).await, (0, 0));

	// A private receipt on a later main-timeline event covers the thread too.
	let private = send(router, &room, HELPER_TOKEN, "private", stream_reply(&thread)).await;
	let latest = send(
		router,
		&room,
		HELPER_TOKEN,
		"latest",
		json!({"msgtype": "m.text", "body": "Meanwhile"}),
	)
	.await;
	request(
		router,
		"POST",
		&format!("/rooms/{room}/read_markers"),
		AUTHOR_TOKEN,
		json!({"m.read.private": latest}),
	)
	.await;
	send(
		router,
		&room,
		HELPER_TOKEN,
		"private-done",
		terminal_reply("completed", &private, &question),
	)
	.await;
	assert_eq!(thread_counts(services, &room, &question).await, (0, 0));

	// A receipt sent mid-stream on an earlier event does not reach the reply.
	let earlier = send(
		router,
		&room,
		HELPER_TOKEN,
		"earlier",
		json!({"msgtype": "m.text", "body": "Meanwhile, again"}),
	)
	.await;
	let later = send(router, &room, HELPER_TOKEN, "later", stream_reply(&thread)).await;
	request(
		router,
		"POST",
		&format!("/rooms/{room}/receipt/m.read/{earlier}"),
		AUTHOR_TOKEN,
		json!({}),
	)
	.await;
	send(
		router,
		&room,
		HELPER_TOKEN,
		"later-done",
		terminal_reply("completed", &later, &question),
	)
	.await;
	assert_eq!(thread_counts(services, &room, &question).await, (1, 1));

	Ok(())
}

/// Only the user's own unthreaded receipt covers a thread: neither their
/// `"main"` receipt nor another member's unthreaded receipt on a later event
/// does.
async fn check_other_receipts(services: &Services, router: &Router) -> Result {
	let (room, question) = thread_room(router, "others").await;
	let thread = json!({"rel_type": "m.thread", "event_id": question});
	let reply = send(router, &room, HELPER_TOKEN, "others", stream_reply(&thread)).await;
	let later = send(
		router,
		&room,
		HELPER_TOKEN,
		"others-later",
		json!({"msgtype": "m.text", "body": "Meanwhile"}),
	)
	.await;
	receipt(router, &room, &later, "main").await;
	request(
		router,
		"POST",
		&format!("/rooms/{room}/receipt/m.read/{later}"),
		HELPER_TOKEN,
		json!({}),
	)
	.await;
	send(
		router,
		&room,
		HELPER_TOKEN,
		"others-done",
		terminal_reply("completed", &reply, &question),
	)
	.await;
	assert_eq!(thread_counts(services, &room, &question).await, (1, 1));

	Ok(())
}

/// The server moves the user's private read marker to each of their own
/// sends without telling their clients, so an explicit private receipt at or
/// before such a send still covers the threads it reaches.
async fn check_private_behind_own_send(services: &Services, router: &Router) -> Result {
	let (room, question) = thread_room(router, "own-send").await;
	let thread = json!({"rel_type": "m.thread", "event_id": question});

	// The user posts in the main timeline, then marks the room read there.
	let reply = send(router, &room, HELPER_TOKEN, "on-own", stream_reply(&thread)).await;
	let own = send(
		router,
		&room,
		AUTHOR_TOKEN,
		"on-own-message",
		json!({"msgtype": "m.text", "body": "Meanwhile"}),
	)
	.await;
	request(
		router,
		"POST",
		&format!("/rooms/{room}/read_markers"),
		AUTHOR_TOKEN,
		json!({"m.read.private": own}),
	)
	.await;
	send(
		router,
		&room,
		HELPER_TOKEN,
		"on-own-done",
		terminal_reply("completed", &reply, &question),
	)
	.await;
	assert_eq!(thread_counts(services, &room, &question).await, (0, 0));

	// A receipt on the reply itself, sent after a newer own send.
	let reply = send(router, &room, HELPER_TOKEN, "behind-own", stream_reply(&thread)).await;
	send(
		router,
		&room,
		AUTHOR_TOKEN,
		"behind-own-message",
		json!({"msgtype": "m.text", "body": "Meanwhile, again"}),
	)
	.await;
	request(
		router,
		"POST",
		&format!("/rooms/{room}/receipt/m.read.private/{reply}"),
		AUTHOR_TOKEN,
		json!({}),
	)
	.await;
	send(
		router,
		&room,
		HELPER_TOKEN,
		"behind-own-done",
		terminal_reply("completed", &reply, &question),
	)
	.await;
	assert_eq!(thread_counts(services, &room, &question).await, (0, 0));

	Ok(())
}

/// A private receipt on the user's own post, sent once a reply that started
/// before the post has finished, reads the reply as a public receipt would.
/// The post moved the private marker there without announcing it, so the
/// receipt only advances the announced one, through `/read_markers` and
/// `/receipt` alike.
async fn check_private_after_completion(services: &Services, router: &Router) -> Result {
	for endpoint in ["read_markers", "receipt"] {
		let name = format!("after-{endpoint}");
		let (room, question) = thread_room(router, &name).await;
		let thread = json!({"rel_type": "m.thread", "event_id": question});
		let reply = send(router, &room, HELPER_TOKEN, &name, stream_reply(&thread)).await;
		let own = send(
			router,
			&room,
			AUTHOR_TOKEN,
			&format!("{name}-message"),
			json!({"msgtype": "m.text", "body": "Meanwhile"}),
		)
		.await;
		send(
			router,
			&room,
			HELPER_TOKEN,
			&format!("{name}-done"),
			terminal_reply("completed", &reply, &question),
		)
		.await;
		assert_eq!(thread_counts(services, &room, &question).await, (1, 1), "{name}: finished");

		if endpoint == "read_markers" {
			let path = format!("/rooms/{room}/read_markers");
			request(router, "POST", &path, AUTHOR_TOKEN, json!({"m.read.private": own})).await;
		} else {
			let path = format!("/rooms/{room}/receipt/m.read.private/{own}");
			request(router, "POST", &path, AUTHOR_TOKEN, json!({})).await;
		}
		assert_eq!(thread_counts(services, &room, &question).await, (0, 0), "{name}: read");
	}

	Ok(())
}

/// An unthreaded receipt on an earlier event leaves a later thread read in
/// place, for public and private receipts alike, sent to `/receipt` or to
/// `/read_markers`, which bounds the reset by its receipt's event too.
async fn check_thread_read_kept(services: &Services, router: &Router) -> Result {
	for (kind, endpoint) in [
		("m.read", "receipt"),
		("m.read.private", "receipt"),
		("m.read", "read_markers"),
		("m.read.private", "read_markers"),
	] {
		let name = format!("{kind}-{endpoint}");
		let (room, question) = thread_room(router, &name).await;
		let earlier = send(
			router,
			&room,
			HELPER_TOKEN,
			&format!("{name}-earlier"),
			json!({"msgtype": "m.text", "body": "Meanwhile"}),
		)
		.await;
		let thread = json!({"rel_type": "m.thread", "event_id": question});
		let reply = send(router, &room, HELPER_TOKEN, &name, stream_reply(&thread)).await;
		request(
			router,
			"POST",
			&format!("/rooms/{room}/receipt/{kind}/{reply}"),
			AUTHOR_TOKEN,
			json!({"thread_id": question}),
		)
		.await;

		if endpoint == "receipt" {
			let path = format!("/rooms/{room}/receipt/{kind}/{earlier}");
			request(router, "POST", &path, AUTHOR_TOKEN, json!({})).await;
		} else {
			let body = if kind == "m.read" {
				json!({"m.read": earlier})
			} else {
				json!({"m.read.private": earlier})
			};
			let path = format!("/rooms/{room}/read_markers");
			request(router, "POST", &path, AUTHOR_TOKEN, body).await;
		}

		send(
			router,
			&room,
			HELPER_TOKEN,
			&format!("{name}-done"),
			terminal_reply("completed", &reply, &question),
		)
		.await;
		assert_eq!(thread_counts(services, &room, &question).await, (0, 0), "{name}");
	}

	Ok(())
}

/// One read of the unthreaded receipts serves every recipient of a finished
/// reply: each one whose receipt reaches it is spared, and each other one is
/// counted, whichever of them is handled first.
async fn check_shared_readers(services: &Services, router: &Router) -> Result {
	let author = user_id!("@author:localhost");
	let observer = user_id!("@observer:localhost");

	for (author_reads, observer_reads) in [(true, false), (false, true), (true, true)] {
		let name = format!("shared-{author_reads}-{observer_reads}");
		let (room, question) = thread_room(router, &name).await;
		request(router, "POST", &format!("/rooms/{room}/join"), OBSERVER_TOKEN, json!({})).await;
		let thread = json!({"rel_type": "m.thread", "event_id": question});
		let reply = send(router, &room, HELPER_TOKEN, &name, stream_reply(&thread)).await;
		let later = send(
			router,
			&room,
			HELPER_TOKEN,
			&format!("{name}-later"),
			json!({"msgtype": "m.text", "body": "Meanwhile"}),
		)
		.await;

		for (reads, token) in [(author_reads, AUTHOR_TOKEN), (observer_reads, OBSERVER_TOKEN)] {
			if reads {
				let path = format!("/rooms/{room}/receipt/m.read/{later}");
				request(router, "POST", &path, token, json!({})).await;
			}
		}

		send(
			router,
			&room,
			HELPER_TOKEN,
			&format!("{name}-done"),
			terminal_reply("completed", &reply, &question),
		)
		.await;

		let expected = |reads: bool, unread: (u64, u64)| if reads { (0, 0) } else { unread };
		assert_eq!(
			thread_counts_of(services, author, &room, &question).await,
			expected(author_reads, (1, 1)),
			"{name}: author"
		);
		assert_eq!(
			thread_counts_of(services, observer, &room, &question).await,
			expected(observer_reads, (1, 0)),
			"{name}: observer"
		);
	}

	Ok(())
}

/// A new room the helper joined, with the author's question to thread on.
async fn thread_room(router: &Router, name: &str) -> (OwnedRoomId, OwnedEventId) {
	let room = create_room(router).await;
	request(router, "POST", &format!("/rooms/{room}/join"), HELPER_TOKEN, json!({})).await;
	let question = send(
		router,
		&room,
		AUTHOR_TOKEN,
		&format!("{name}-question"),
		json!({"msgtype": "m.text", "body": "What is the answer?"}),
	)
	.await;

	(room, question)
}

async fn receipt(router: &Router, room: &RoomId, event: &EventId, thread_id: &str) {
	request(
		router,
		"POST",
		&format!("/rooms/{room}/receipt/m.read/{event}"),
		AUTHOR_TOKEN,
		json!({"thread_id": thread_id}),
	)
	.await;
}

async fn thread_counts(services: &Services, room: &RoomId, root: &EventId) -> (u64, u64) {
	thread_counts_of(services, user_id!("@author:localhost"), room, root).await
}

async fn thread_counts_of(
	services: &Services,
	user: &UserId,
	room: &RoomId,
	root: &EventId,
) -> (u64, u64) {
	services
		.pusher
		.thread_notification_counts(user, room)
		.await
		.get(root)
		.copied()
		.unwrap_or_default()
}

fn encrypted_stream(status: &str, relation: &Value) -> Value {
	json!({
		"algorithm": "m.megolm.v1.aes-sha2",
		"ciphertext": "opaque",
		"device_id": "HELPERDEVICE",
		"sender_key": "sender-key",
		"session_id": "session",
		"io.mindroom.stream_status": status,
		"m.relates_to": relation,
	})
}

fn stream_reply(thread: &Value) -> Value {
	json!({
		"msgtype": "m.text",
		"body": "Thinking",
		"io.mindroom.stream_status": "streaming",
		"m.relates_to": thread,
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
		"m.new_content": {
			"msgtype": "m.text",
			"body": "Final answer",
			"m.relates_to": {"m.in_reply_to": {"event_id": target}},
		},
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
