mod support;

#[cfg(test)]
mod tests {
	use std::sync::Arc;

	use axum::Router;
	use serde_json::{Value, json};
	use tuwunel_core::{Result, http::StatusCode, ruma::user_id, utils::millis_since_unix_epoch};
	use tuwunel_service::Services;

	use super::support::{
		Harness,
		openid::{
			REQUEST_TOKEN_PATH, USER, assert_userinfo, get, post, request_token, send, setup,
			userinfo_uri,
		},
	};

	const AUDIENCE: &str = "https://mindroom.example.test";
	const OTHER_AUDIENCE: &str = "https://other.example.test";
	const STORED_TOKENS: &str = "openidtoken_expiresatuserid";

	#[test]
	fn openid_tokens_bind_to_the_requested_audience() -> Result {
		let harness = Harness::new("mindroom_openid_audience", [
			"mindroom_openid_audience=true".to_owned(),
		])?;

		harness.with_services(async |services| {
			let (router, _guard) = setup(&services).await?;

			advertises_the_capability(&router).await;
			verifies_per_audience_table(&router).await;
			rejects_invalid_audiences(&router).await;
			rejects_non_canonical_bodies(&router).await;
			stored_rows_keep_their_format(&services).await
		})
	}

	async fn advertises_the_capability(router: &Router) {
		let (status, body) = send(router, get("/_matrix/client/versions")).await;
		assert_eq!(status, StatusCode::OK, "versions: {body}");
		assert_eq!(
			body["unstable_features"]["io.mindroom.openid_audience"],
			Value::Bool(true),
			"versions must advertise audience-bound OpenID tokens: {body}",
		);
	}

	async fn verifies_per_audience_table(router: &Router) {
		let bound = request_token(router, Some(json!({"io.mindroom.audience": AUDIENCE}))).await;
		let unbound = request_token(router, Some(json!({}))).await;
		let no_body = request_token(router, None).await;

		// Rejected attempts come first: they must not consume the token.
		assert_userinfo(router, &bound, Some(OTHER_AUDIENCE), StatusCode::UNAUTHORIZED).await;
		assert_userinfo(router, &bound, None, StatusCode::UNAUTHORIZED).await;
		assert_userinfo(router, &bound, Some(""), StatusCode::UNAUTHORIZED).await;
		assert_userinfo(router, &bound, Some(AUDIENCE), StatusCode::OK).await;

		assert_userinfo(router, &unbound, Some(AUDIENCE), StatusCode::UNAUTHORIZED).await;
		assert_userinfo(router, &unbound, None, StatusCode::OK).await;

		assert_userinfo(router, &no_body, Some(AUDIENCE), StatusCode::UNAUTHORIZED).await;
		assert_userinfo(router, &no_body, None, StatusCode::OK).await;

		// A repeated audience is not read as either value.
		let uri = userinfo_uri(&[
			("access_token", &unbound),
			("io.mindroom.audience", AUDIENCE),
			("io.mindroom.audience", AUDIENCE),
		]);
		let (status, body) = send(router, get(&uri)).await;
		assert_eq!(status, StatusCode::BAD_REQUEST, "repeated audience: {body}");
		assert_eq!(body["errcode"], "M_INVALID_PARAM", "repeated audience: {body}");

		// The longest audience counts characters, not bytes.
		let longest = "é".repeat(255);
		let token =
			request_token(router, Some(json!({"io.mindroom.audience": longest.clone()}))).await;
		assert_userinfo(router, &token, Some(&longest), StatusCode::OK).await;
	}

	async fn rejects_invalid_audiences(router: &Router) {
		let invalid = [
			json!(1),
			json!(null),
			json!(true),
			json!([AUDIENCE]),
			json!({"origin": AUDIENCE}),
			json!(""),
			json!("a".repeat(256)),
			json!("https://mindroom.example.test\n"),
			json!("https://mindroom.example.test\u{7f}"),
		];

		for audience in invalid {
			let request =
				post(REQUEST_TOKEN_PATH, Some(json!({"io.mindroom.audience": audience.clone()})));
			let (status, body) = send(router, request).await;
			assert_eq!(status, StatusCode::BAD_REQUEST, "audience {audience}: {body}");
			assert_eq!(body["errcode"], "M_INVALID_PARAM", "audience {audience}: {body}");
		}
	}

	async fn rejects_non_canonical_bodies(router: &Router) {
		// Valid JSON that is not canonical JSON leaves the audience unreadable,
		// so no token is issued rather than an unbound one.
		let bodies = [
			json!({"io.mindroom.audience": 1.5}),
			json!({"io.mindroom.audience": 9_007_199_254_740_992_u64}),
			json!({"io.mindroom.audience": AUDIENCE, "extra": 1.5}),
		];

		for request_body in bodies {
			let (status, body) =
				send(router, post(REQUEST_TOKEN_PATH, Some(request_body.clone()))).await;
			assert_eq!(status, StatusCode::BAD_REQUEST, "body {request_body}: {body}");
			assert_eq!(body["errcode"], "M_BAD_JSON", "body {request_body}: {body}");
		}
	}

	async fn stored_rows_keep_their_format(services: &Arc<Services>) -> Result {
		let user = user_id!("@alice:localhost");
		let expires_at = millis_since_unix_epoch()
			.saturating_add(60_000)
			.to_be_bytes();

		// Rows written before audiences existed have no separator and stay unbound.
		let mut legacy = expires_at.to_vec();
		legacy.extend_from_slice(USER.as_bytes());
		services.db[STORED_TOKENS].insert("legacy-openid-token", legacy);
		assert!(
			services
				.users
				.find_from_openid_token("legacy-openid-token", Some(AUDIENCE))
				.await
				.is_err(),
			"a legacy row must not verify for an audience",
		);
		assert_eq!(
			services
				.users
				.find_from_openid_token("legacy-openid-token", None)
				.await?,
			user,
			"a legacy row must verify as unbound",
		);

		// A bound token stores the audience after the user ID and a 0xFF byte.
		services
			.users
			.create_openid_token(user, "bound-openid-token", Some(AUDIENCE))?;
		let stored = services.db[STORED_TOKENS]
			.get("bound-openid-token")
			.await?
			.to_vec();
		let (_, user_and_audience) = stored.split_at(8);
		let mut expected = USER.as_bytes().to_vec();
		expected.push(0xFF);
		expected.extend_from_slice(AUDIENCE.as_bytes());
		assert_eq!(user_and_audience, expected.as_slice(), "bound row encoding");

		services
			.users
			.create_openid_token(user, "unbound-openid-token", None)?;
		let stored = services.db[STORED_TOKENS]
			.get("unbound-openid-token")
			.await?
			.to_vec();
		let (_, user_only) = stored.split_at(8);
		assert_eq!(user_only, USER.as_bytes(), "unbound row encoding");

		// Expired bound tokens are refused and removed like unbound ones.
		let mut expired = millis_since_unix_epoch()
			.saturating_sub(1)
			.to_be_bytes()
			.to_vec();
		expired.extend_from_slice(USER.as_bytes());
		expired.push(0xFF);
		expired.extend_from_slice(AUDIENCE.as_bytes());
		services.db[STORED_TOKENS].insert("expired-openid-token", expired);
		assert!(
			services
				.users
				.find_from_openid_token("expired-openid-token", Some(AUDIENCE))
				.await
				.is_err(),
			"an expired bound token must not verify",
		);
		assert!(
			services.db[STORED_TOKENS]
				.get("expired-openid-token")
				.await
				.is_err(),
			"an expired token must be removed",
		);

		Ok(())
	}
}
