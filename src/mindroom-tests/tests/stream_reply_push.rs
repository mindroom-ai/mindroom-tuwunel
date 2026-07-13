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
	Result,
	http::{Request, StatusCode, header},
	ruma::{
		EventId, OwnedEventId, OwnedRoomId, RoomId, UserId,
		api::client::push::{Pusher, PusherIds, PusherInit, PusherKind},
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

		exercise_replies(&services, &router, author, &room, &pusher, &mut captured).await
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
	let response = request(
		router,
		"PUT",
		&format!("/rooms/{room}/send/m.room.message/{txn}"),
		token,
		content,
	)
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
