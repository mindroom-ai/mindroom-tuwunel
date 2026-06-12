mod support;

#[cfg(test)]
mod tests {
	use axum::{Router, body::Body};
	use serde_json::{Value as JsonValue, json};
	use tower::ServiceExt;
	use tuwunel_core::{
		Result,
		http::{Request, StatusCode, header},
		ruma::{OwnedRoomId, events::StateEventType, user_id},
	};

	use super::support::Harness;

	const ACCESS_TOKEN: &str = "mindroom-test-access-token-0123456789abcdef";

	/// `default_power_level_content_override` must flow from the server config
	/// through `apply_power_levels_pdu` into the created room's
	/// `m.room.power_levels` state, with a request-level override still
	/// winning. This pins the call-site threading that rebases keep moving
	/// around (upstream split `create_room_route` into helpers in v1.7.1).
	#[test]
	fn create_room_applies_default_power_level_content_override() -> Result {
		let mut harness = Harness::new("mindroom_rebase_power_levels", [])?;
		harness.args.option.push(
			"default_power_level_content_override={events_default=51,events={\"com.mindroom.\
			 test.event\"=77}}"
				.to_owned(),
		);

		harness.with_services(|services| async move {
			let user_id = user_id!("@power_user:localhost");
			services
				.users
				.create(user_id, Some("password"), None)
				.await?;
			services
				.users
				.create_device(user_id, None, (Some(ACCESS_TOKEN), None), None, None, None)
				.await?;

			let (state, _guard) = tuwunel_api::router::state::create(services.clone());
			let router =
				tuwunel_api::router::build(Router::new(), &services.server).with_state(state);

			// Room 1: no request override; server default must apply.
			let room_id = create_room(&router, json!({})).await?;
			let power_levels = services
				.state_accessor
				.room_state_get_content::<JsonValue>(
					&room_id,
					&StateEventType::RoomPowerLevels,
					"",
				)
				.await?;

			assert_eq!(
				power_levels.get("events_default"),
				Some(&json!(51)),
				"server default override should set events_default: {power_levels}",
			);
			assert_eq!(
				power_levels
					.get("events")
					.and_then(|events| events.get("com.mindroom.test.event")),
				Some(&json!(77)),
				"server default override should merge into the events map: {power_levels}",
			);
			assert_eq!(
				power_levels
					.get("users")
					.and_then(|users| users.get(user_id.as_str())),
				Some(&json!(100)),
				"creator power level should survive the override merge: {power_levels}",
			);

			// Room 2: request-level override must win over the server default.
			let room_id = create_room(
				&router,
				json!({"power_level_content_override": {"events_default": 75}}),
			)
			.await?;
			let power_levels = services
				.state_accessor
				.room_state_get_content::<JsonValue>(
					&room_id,
					&StateEventType::RoomPowerLevels,
					"",
				)
				.await?;

			assert_eq!(
				power_levels.get("events_default"),
				Some(&json!(75)),
				"request override should win over the server default: {power_levels}",
			);
			assert_eq!(
				power_levels
					.get("events")
					.and_then(|events| events.get("com.mindroom.test.event")),
				Some(&json!(77)),
				"server default for untouched keys should still apply: {power_levels}",
			);

			Ok(())
		})
	}

	async fn create_room(router: &Router, body: JsonValue) -> Result<OwnedRoomId> {
		let response = router
			.clone()
			.oneshot(
				Request::builder()
					.method("POST")
					.uri("/_matrix/client/v3/createRoom")
					.header(header::AUTHORIZATION, format!("Bearer {ACCESS_TOKEN}"))
					.header(header::CONTENT_TYPE, "application/json")
					.header("X-Forwarded-For", "127.0.0.1")
					.body(Body::from(body.to_string()))
					.expect("valid request"),
			)
			.await
			.expect("router response");

		let status = response.status();
		let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
			.await
			.expect("readable createRoom response body");
		assert_eq!(
			status,
			StatusCode::OK,
			"createRoom should succeed: {}",
			String::from_utf8_lossy(&bytes),
		);

		let body: JsonValue =
			serde_json::from_slice(&bytes).expect("createRoom response should be JSON");
		let room_id = body
			.get("room_id")
			.and_then(JsonValue::as_str)
			.unwrap_or_else(|| panic!("createRoom response missing room_id: {body}"));

		Ok(OwnedRoomId::try_from(room_id).expect("valid room_id"))
	}
}
