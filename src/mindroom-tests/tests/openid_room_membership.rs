mod support;

#[cfg(test)]
mod tests {
	use serde_json::json;
	use tuwunel_core::{Result, http::StatusCode};

	use super::support::{
		Harness,
		openid::{USER, get, post, request_token, send, setup, userinfo_uri},
	};

	/// With `io.mindroom.room_id`, userinfo answers only while the token's user
	/// is joined to that room and says so in the response.
	#[test]
	fn openid_userinfo_checks_room_membership() -> Result {
		let harness = Harness::new("mindroom_openid_room_membership", [])?;

		harness.with_services(async |services| {
			let (router, _guard) = setup(&services).await?;
			let token = request_token(&router, Some(json!({}))).await;

			let (status, created) =
				send(&router, post("/_matrix/client/v3/createRoom", Some(json!({})))).await;
			assert_eq!(status, StatusCode::OK, "createRoom: {created}");
			let room_id = created["room_id"]
				.as_str()
				.expect("createRoom returns room_id");

			let plain = userinfo_uri(&[("access_token", &token)]);
			let (status, body) = send(&router, get(&plain)).await;
			assert_eq!(status, StatusCode::OK, "userinfo: {body}");
			assert_eq!(body, json!({"sub": USER}), "userinfo without a room");

			let in_room =
				userinfo_uri(&[("access_token", &token), ("io.mindroom.room_id", room_id)]);
			let (status, body) = send(&router, get(&in_room)).await;
			assert_eq!(status, StatusCode::OK, "joined: {body}");
			assert_eq!(body, json!({"sub": USER, "io.mindroom.room_joined": true}), "joined");

			let leave = format!("/_matrix/client/v3/rooms/{room_id}/leave");
			let (status, body) = send(&router, post(&leave, Some(json!({})))).await;
			assert_eq!(status, StatusCode::OK, "leave: {body}");

			let (status, body) = send(&router, get(&in_room)).await;
			assert_eq!(status, StatusCode::FORBIDDEN, "after leaving: {body}");
			assert_eq!(body["errcode"], "M_FORBIDDEN", "after leaving: {body}");

			Ok(())
		})
	}
}
