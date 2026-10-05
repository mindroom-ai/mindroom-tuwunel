mod support;

#[cfg(test)]
mod tests {
	use std::sync::{
		Arc,
		atomic::{AtomicU64, Ordering},
	};

	use axum::{Extension, Router, body::Body};
	use serde_json::{Value, json};
	use tokio::sync::Notify;
	use tower::ServiceExt;
	use tuwunel_core::{
		Result,
		http::{Method, Request, StatusCode, header},
		ruma::{device_id, user_id},
	};

	use super::support::Harness;

	const TOKEN: &str = "sync-edit-compaction-access-token-0123456789";

	static NEXT_TXN: AtomicU64 = AtomicU64::new(1);

	/// Drive the real router through legacy and sliding sync and pin the edit
	/// compaction both share: a superseded `m.replace` is left out of the
	/// timeline while the newest edit, the original, and a state event that
	/// claims a replacement relation are all kept, on an initial and an
	/// incremental sync alike.
	#[test]
	fn sync_timelines_keep_only_the_newest_edit() -> Result {
		let harness = Harness::new("mindroom_sync_edit_compaction", [
			"mindroom_compact_edits_enabled=true".to_owned(),
			"mindroom_edit_purge_enabled=false".to_owned(),
		])?;

		harness.with_services(async |services| {
			let user_id = user_id!("@alice:localhost");
			services
				.users
				.create(user_id, Some("password"), None)
				.await?;
			services
				.users
				.create_device(
					user_id,
					Some(device_id!("COMPACT")),
					(Some(TOKEN), None),
					None,
					None,
					None,
				)
				.await?;

			let (state, _guard) = tuwunel_api::router::state::create(services.clone());
			// The request layer gives every request the notifier sliding sync
			// waits on; this router is driven without that layer.
			let router = tuwunel_api::router::build(Router::new(), &services.server)
				.with_state(state)
				.layer(Extension(Arc::new(Notify::new())));

			let (status, created) =
				request(&router, Method::POST, "/_matrix/client/v3/createRoom", json!({})).await;
			assert_eq!(status, StatusCode::OK, "createRoom: {created}");
			let room = created["room_id"]
				.as_str()
				.expect("created room ID")
				.to_owned();

			let original =
				send_message(&router, &room, json!({"msgtype": "m.text", "body": "v0"})).await;
			let edit1 = send_edit(&router, &room, &original, "v1").await;
			let topic = send_topic(&router, &room, &original, "relating topic").await;
			let edit2 = send_edit(&router, &room, &original, "v2").await;
			let edit3 = send_edit(&router, &room, &original, "v3").await;

			let initial = sync_v3(&router, None).await;
			let timeline = v3_timeline(&initial, &room);
			assert_contains(&timeline, &[&original, &topic, &edit3], "initial legacy sync");
			assert_omits(&timeline, &[&edit1, &edit2], "initial legacy sync");
			assert_order(&timeline, &[&original, &topic, &edit3], "initial legacy sync");

			let sliding = sync_v5(&router, &room, None).await;
			let timeline = event_ids(&sliding["rooms"][&room]["timeline"]);
			assert_contains(&timeline, &[&original, &topic, &edit3], "sliding sync");
			assert_omits(&timeline, &[&edit1, &edit2], "sliding sync");
			assert_order(&timeline, &[&original, &topic, &edit3], "sliding sync");

			let since = initial["next_batch"]
				.as_str()
				.expect("next_batch")
				.to_owned();
			let edit4 = send_edit(&router, &room, &original, "v4").await;
			let topic2 = send_topic(&router, &room, &original, "second relating topic").await;
			let edit5 = send_edit(&router, &room, &original, "v5").await;

			let incremental = sync_v3(&router, Some(&since)).await;
			let timeline = v3_timeline(&incremental, &room);
			assert_contains(&timeline, &[&topic2, &edit5], "incremental legacy sync");
			assert_omits(&timeline, &[&edit4], "incremental legacy sync");
			assert_order(&timeline, &[&topic2, &edit5], "incremental legacy sync");

			let pos = sliding["pos"].as_str().expect("sliding sync pos");
			let sliding = sync_v5(&router, &room, Some(pos)).await;
			let timeline = event_ids(&sliding["rooms"][&room]["timeline"]);
			assert_contains(&timeline, &[&topic2, &edit5], "incremental sliding sync");
			assert_omits(&timeline, &[&edit4], "incremental sliding sync");
			assert_order(&timeline, &[&topic2, &edit5], "incremental sliding sync");

			// The relating topic is current room state, so the client must see it.
			let topic_state = incremental["rooms"]["join"][&room]["timeline"]["events"]
				.as_array()
				.expect("incremental timeline events")
				.iter()
				.find(|event| event["event_id"] == topic2.as_str())
				.expect("relating topic in the incremental timeline");
			assert_eq!(topic_state["type"], "m.room.topic");
			assert_eq!(topic_state["state_key"], "");

			Ok(())
		})
	}

	async fn send_message(router: &Router, room: &str, content: Value) -> String {
		let txn = NEXT_TXN.fetch_add(1, Ordering::Relaxed);
		let path = format!(
			"/_matrix/client/v3/rooms/{}/send/m.room.message/compaction-{txn}",
			encode(room)
		);
		let (status, body) = request(router, Method::PUT, &path, content).await;
		assert_eq!(status, StatusCode::OK, "send message: {body}");

		body["event_id"]
			.as_str()
			.expect("sent event ID")
			.to_owned()
	}

	async fn send_edit(router: &Router, room: &str, original: &str, body: &str) -> String {
		send_message(
			router,
			room,
			json!({
				"msgtype": "m.text",
				"body": format!("* {body}"),
				"m.new_content": {"msgtype": "m.text", "body": body},
				"m.relates_to": {"rel_type": "m.replace", "event_id": original},
			}),
		)
		.await
	}

	async fn send_topic(router: &Router, room: &str, original: &str, topic: &str) -> String {
		let path = format!("/_matrix/client/v3/rooms/{}/state/m.room.topic/", encode(room));
		let content = json!({
			"topic": topic,
			"m.relates_to": {"rel_type": "m.replace", "event_id": original},
		});
		let (status, body) = request(router, Method::PUT, &path, content).await;
		assert_eq!(status, StatusCode::OK, "send topic: {body}");

		body["event_id"]
			.as_str()
			.expect("topic event ID")
			.to_owned()
	}

	async fn sync_v3(router: &Router, since: Option<&str>) -> Value {
		let filter = encode(r#"{"room":{"timeline":{"limit":50}}}"#);
		let since = since.map_or_else(String::new, |since| format!("&since={}", encode(since)));
		let path = format!("/_matrix/client/v3/sync?timeout=0&filter={filter}{since}");
		let (status, body) = request(router, Method::GET, &path, Value::Null).await;
		assert_eq!(status, StatusCode::OK, "legacy sync: {body}");

		body
	}

	async fn sync_v5(router: &Router, room: &str, pos: Option<&str>) -> Value {
		let pos = pos.map_or_else(String::new, |pos| format!("&pos={}", encode(pos)));
		let path =
			format!("/_matrix/client/unstable/org.matrix.simplified_msc3575/sync?timeout=0{pos}");
		let body = json!({
			"lists": {},
			"room_subscriptions": {room: {"required_state": [], "timeline_limit": 50}},
		});
		let (status, body) = request(router, Method::POST, &path, body).await;
		assert_eq!(status, StatusCode::OK, "sliding sync: {body}");

		body
	}

	fn v3_timeline(sync: &Value, room: &str) -> Vec<String> {
		event_ids(&sync["rooms"]["join"][room]["timeline"]["events"])
	}

	fn event_ids(events: &Value) -> Vec<String> {
		events
			.as_array()
			.unwrap_or_else(|| panic!("timeline events: {events}"))
			.iter()
			.map(|event| {
				event["event_id"]
					.as_str()
					.expect("timeline event ID")
					.to_owned()
			})
			.collect()
	}

	fn assert_contains(timeline: &[String], expected: &[&String], context: &str) {
		for event_id in expected {
			assert!(
				timeline.contains(event_id),
				"{context}: {event_id} missing from {timeline:?}",
			);
		}
	}

	fn assert_omits(timeline: &[String], superseded: &[&String], context: &str) {
		for event_id in superseded {
			assert!(
				!timeline.contains(event_id),
				"{context}: superseded {event_id} kept in {timeline:?}",
			);
		}
	}

	fn assert_order(timeline: &[String], expected: &[&String], context: &str) {
		let positions: Vec<_> = expected
			.iter()
			.map(|event_id| timeline.iter().position(|id| id == *event_id))
			.collect();

		assert!(positions.is_sorted(), "{context}: {expected:?} out of order in {timeline:?}");
	}

	fn encode(value: &str) -> String {
		url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
	}

	async fn request(
		router: &Router,
		method: Method,
		path: &str,
		body: Value,
	) -> (StatusCode, Value) {
		let body = if body.is_null() {
			Body::empty()
		} else {
			Body::from(body.to_string())
		};
		let request = Request::builder()
			.method(method)
			.uri(path)
			.header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
			.header(header::CONTENT_TYPE, "application/json")
			.header("X-Forwarded-For", "127.0.0.1")
			.body(body)
			.expect("valid request");

		let response = router
			.clone()
			.oneshot(request)
			.await
			.expect("router response");
		let status = response.status();
		let bytes = axum::body::to_bytes(response.into_body(), 1 << 22)
			.await
			.expect("readable response body");
		let body = serde_json::from_slice(&bytes)
			.unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));

		(status, body)
	}
}
