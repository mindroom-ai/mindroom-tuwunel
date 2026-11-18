mod support;

#[cfg(test)]
mod tests {
	use std::time::{Duration, Instant};

	use axum::Router;
	use serde_json::{Value, json};
	use tuwunel_core::{
		Result,
		http::StatusCode,
		ruma::{device_id, user_id},
	};

	use super::support::{
		Harness,
		openid::{authed_get, post, post_as, put, send, setup},
	};

	const CALL_MEMBER: &str = "org.matrix.msc3401.call.member";
	const STATE_KEY: &str = "_%40alice%3Alocalhost_OPENID";
	const BOB_TOKEN: &str = "delayed-events-bob-access-token-0123456789";

	async fn call_member(router: &Router, state: &str) -> Value {
		let (status, content) = send(router, authed_get(state)).await;
		assert_eq!(status, StatusCode::OK, "call member state: {content}");
		content
	}

	async fn schedule_leave(router: &Router, state: &str, millis: u64) -> String {
		let delayed = format!("{state}?org.matrix.msc4140.delay={millis}");
		let (status, body) = send(router, put(&delayed, &json!({}))).await;
		assert_eq!(status, StatusCode::OK, "delayed leave: {body}");
		body["delay_id"]
			.as_str()
			.expect("delayed leave returns delay_id")
			.to_owned()
	}

	fn update(delay_id: &str, action: &str) -> String {
		format!("/_matrix/client/unstable/org.matrix.msc4140/delayed_events/{delay_id}/{action}")
	}

	/// Each request is refused with 403 `M_FORBIDDEN`.
	async fn assert_not_delayed(router: &Router, requests: &[(&str, &str, Value)]) {
		for (what, uri, body) in requests {
			let (status, body) = send(router, put(uri, body)).await;
			assert_eq!(status, StatusCode::FORBIDDEN, "delayed {what}: {body}");
			assert_eq!(body["errcode"], "M_FORBIDDEN", "delayed {what}: {body}");
		}
	}

	/// A MatrixRTC client schedules its own leave and keeps restarting it, in
	/// the path form matrix-js-sdk calls or the body form matrix-rust-sdk calls;
	/// when the client stops, as on a crash, the server sends the leave. The
	/// client can also cancel the leave or send it at once, and nobody else can
	/// touch it. Nothing else can be delayed, by any request form: no other
	/// state, such as a room membership, no message, no call membership with
	/// content, such as a join, and no leave outside the sender's own slot. Nor
	/// can a suspended account's leave. `m.call.member` is the same event type
	/// under its other name.
	#[test]
	fn delayed_call_member_leave_is_sent_once_restarts_stop() -> Result {
		let harness = Harness::new("mindroom_delayed_call_membership", [])?;

		harness.with_services(async |services| {
			let (router, _guard) = setup(&services).await?;
			let bob = user_id!("@bob:localhost");
			services
				.users
				.create(bob, Some("password"), None)
				.await?;
			services
				.users
				.create_device(
					bob,
					Some(device_id!("BOB")),
					(Some(BOB_TOKEN), None),
					None,
					None,
					None,
				)
				.await?;

			let (status, created) =
				send(&router, post("/_matrix/client/v3/createRoom", Some(json!({})))).await;
			assert_eq!(status, StatusCode::OK, "createRoom: {created}");
			let room_id = created["room_id"]
				.as_str()
				.expect("createRoom returns room_id");

			let rooms = format!("/_matrix/client/v3/rooms/{room_id}");
			let unstable = format!("/_matrix/client/unstable/org.matrix.msc4140/rooms/{room_id}");
			let delay = "org.matrix.msc4140.delay=1000";
			let member = format!("{rooms}/state/m.room.member/%40alice%3Alocalhost?{delay}");
			let topic = format!("{rooms}/state/m.room.topic?{delay}");
			let message = format!("{rooms}/send/m.room.message/query?{delay}");
			let stable = format!("{rooms}/delayed_event/m.room.message/stable");
			let dedicated = format!("{unstable}/delayed_event/m.room.topic/dedicated");
			let text = json!({"msgtype": "m.text", "body": "later"});
			let state =
				format!("/_matrix/client/v3/rooms/{room_id}/state/{CALL_MEMBER}/{STATE_KEY}");
			let joined = json!({"application": "m.call", "call_id": "", "device_id": "OPENID"});
			let call = format!("{unstable}/delayed_event/{CALL_MEMBER}/call");
			let call_by_body = |state_key: &str, content: &Value| {
				json!({"delay_ms": 1000, "state_key": state_key, "content": content})
			};
			let slot = |key: &str| format!("{rooms}/state/{CALL_MEMBER}/{key}?{delay}");
			assert_not_delayed(&router, &[
				("membership", &member, json!({"membership": "leave"})),
				("empty-key state", &topic, json!({"topic": "later"})),
				("message", &message, text.clone()),
				("message by body", &stable, json!({"delay_ms": 1000, "content": text})),
				("state by body", &dedicated, call_by_body("", &json!({"topic": "later"}))),
				("call join", &slot(STATE_KEY), joined.clone()),
				("call join by body", &call, call_by_body("_@alice:localhost_OPENID", &joined)),
				("empty-key leave", &slot(""), json!({})),
				("Bob's leave", &slot("_%40bob%3Alocalhost_BOB"), json!({})),
				("Bob's leave by body", &call, call_by_body("_@bob:localhost_BOB", &json!({}))),
				("leave of a look-alike slot", &slot("_%40alice%3Alocalhost2_OPENID"), json!({})),
				("leave without a slot", &slot("%40alice%3Alocalhost"), json!({})),
			])
			.await;

			let (status, body) = send(&router, put(&state, &joined)).await;
			assert_eq!(status, StatusCode::OK, "join: {body}");

			let delay_id = schedule_leave(&router, &state, 2000).await;

			tokio::time::sleep(Duration::from_millis(1200)).await;
			let restart = update(&delay_id, "restart");
			let (status, body) = send(&router, post(&restart, None)).await;
			assert_eq!(status, StatusCode::OK, "restart: {body}");

			tokio::time::sleep(Duration::from_millis(1200)).await;
			let restart_in_body =
				format!("/_matrix/client/unstable/org.matrix.msc4140/delayed_events/{delay_id}");
			let (status, body) =
				send(&router, post(&restart_in_body, Some(json!({"action": "restart"})))).await;
			assert_eq!(status, StatusCode::OK, "restart in body: {body}");

			tokio::time::sleep(Duration::from_millis(1200)).await;
			assert_eq!(call_member(&router, &state).await, joined, "restarts postpone the leave");

			let deadline = Instant::now() + Duration::from_secs(10);
			while call_member(&router, &state).await != json!({}) {
				assert!(Instant::now() < deadline, "the delayed leave was never sent");
				tokio::time::sleep(Duration::from_millis(50)).await;
			}

			let (status, body) = send(&router, put(&state, &joined)).await;
			assert_eq!(status, StatusCode::OK, "rejoin: {body}");
			let delay_id = schedule_leave(&router, &state, 60_000).await;
			let (status, body) =
				send(&router, post_as(BOB_TOKEN, &update(&delay_id, "send"), None)).await;
			assert_eq!(status, StatusCode::NOT_FOUND, "another user's send: {body}");
			let (status, body) = send(&router, post(&update(&delay_id, "cancel"), None)).await;
			assert_eq!(status, StatusCode::OK, "cancel: {body}");
			assert_eq!(
				call_member(&router, &state).await,
				joined,
				"a cancelled leave is not sent"
			);

			// matrix-js-sdk sends the scheduled leave at once on a normal hang-up.
			let delay_id = schedule_leave(&router, &state, 60_000).await;
			let (status, body) = send(&router, post(&update(&delay_id, "send"), None)).await;
			assert_eq!(status, StatusCode::OK, "send: {body}");
			assert_eq!(call_member(&router, &state).await, json!({}), "send leaves at once");

			// ruma reads `m.call.member` as an alias of the call membership type, so
			// the request delays, and the server sends, a leave of that type.
			let (status, body) = send(&router, put(&state, &joined)).await;
			assert_eq!(status, StatusCode::OK, "rejoin: {body}");
			let alias = format!("{rooms}/state/m.call.member/{STATE_KEY}?{delay}");
			let (status, body) = send(&router, put(&alias, &json!({}))).await;
			assert_eq!(status, StatusCode::OK, "delayed m.call.member: {body}");
			let delay_id = body["delay_id"]
				.as_str()
				.expect("delayed m.call.member returns delay_id");
			let record =
				format!("/_matrix/client/unstable/org.matrix.msc4140/delayed_events/{delay_id}");
			let (status, body) = send(&router, authed_get(&record)).await;
			assert_eq!(status, StatusCode::OK, "delayed m.call.member record: {body}");
			assert_eq!(body["type"], CALL_MEMBER, "delayed m.call.member record: {body}");
			let (status, body) = send(&router, post(&update(delay_id, "send"), None)).await;
			assert_eq!(status, StatusCode::OK, "send m.call.member: {body}");
			assert_eq!(call_member(&router, &state).await, json!({}), "m.call.member is sent");

			// matrix-js-sdk 41.7.0 ends the slot's state key with the call.
			let with_call = format!("{state}_m.call");
			let delay_id = schedule_leave(&router, &with_call, 60_000).await;
			let (status, body) = send(&router, post(&update(&delay_id, "cancel"), None)).await;
			assert_eq!(status, StatusCode::OK, "cancel: {body}");

			let delayed = format!("{state}?org.matrix.msc4140.delay=1000");
			let alice = user_id!("@alice:localhost");
			services.users.set_suspended(alice, alice);
			let (status, body) = send(&router, put(&delayed, &json!({}))).await;
			assert_eq!(status, StatusCode::FORBIDDEN, "suspended: {body}");
			assert_eq!(body["errcode"], "M_USER_SUSPENDED", "suspended: {body}");

			Ok(())
		})
	}
}
