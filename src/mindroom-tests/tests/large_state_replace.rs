//! A local state event larger than half the PDU size limit can be replaced:
//! the limit applies to the PDU, not to the `unsigned.prev_content` copy of
//! the state it replaces.

mod support;

#[cfg(test)]
mod tests {
	use serde_json::json;
	use tuwunel_core::{Result, http::StatusCode};

	use super::support::{
		Harness,
		openid::{authed_get, post, put, send, setup},
	};

	#[test]
	fn large_state_event_can_be_replaced() -> Result {
		let harness = Harness::new("mindroom_large_state_replace", [])?;

		harness.with_services(async |services| {
			let (router, _guard) = setup(&services).await?;
			let (status, created) =
				send(&router, post("/_matrix/client/v3/createRoom", Some(json!({})))).await;
			assert_eq!(status, StatusCode::OK, "createRoom: {created}");
			let room_id = created["room_id"]
				.as_str()
				.expect("createRoom returns room_id");
			let state = format!("/_matrix/client/v3/rooms/{room_id}/state/io.mindroom.large/key");

			let first = json!({"data": "a".repeat(40_000)});
			let (status, body) = send(&router, put(&state, &first)).await;
			assert_eq!(status, StatusCode::OK, "first write: {body}");

			let second = json!({"data": "b".repeat(40_000)});
			let (status, body) = send(&router, put(&state, &second)).await;
			assert_eq!(status, StatusCode::OK, "replacement: {body}");
			let event_id = body["event_id"]
				.as_str()
				.expect("state PUT returns event_id");

			let (status, body) = send(&router, authed_get(&state)).await;
			assert_eq!(status, StatusCode::OK, "state: {body}");
			assert_eq!(body, second, "current state is the replacement");

			let event = format!("/_matrix/client/v3/rooms/{room_id}/event/{event_id}");
			let (status, body) = send(&router, authed_get(&event)).await;
			assert_eq!(status, StatusCode::OK, "event: {body}");
			assert_eq!(body["unsigned"]["prev_content"], first, "replacement keeps prev_content");

			let oversized = json!({"data": "c".repeat(65_536)});
			let (status, body) = send(&router, put(&state, &oversized)).await;
			assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "oversized: {body}");
			assert_eq!(body["errcode"], "M_TOO_LARGE", "oversized: {body}");

			Ok(())
		})
	}
}
