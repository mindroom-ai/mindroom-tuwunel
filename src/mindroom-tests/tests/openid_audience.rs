mod support;

#[cfg(test)]
mod tests {
	use std::sync::Arc;

	use axum::{Router, body::Body};
	use serde_json::{Value, json};
	use tower::ServiceExt;
	use tuwunel_core::{
		Result,
		http::{Request, StatusCode, header},
		ruma::{device_id, user_id},
		utils::millis_since_unix_epoch,
	};
	use tuwunel_service::Services;
	use url::form_urlencoded;

	use super::support::Harness;

	const USER: &str = "@alice:localhost";
	const ACCESS_TOKEN: &str = "openid-audience-access-token-0123456789";
	const AUDIENCE: &str = "https://mindroom.example.test";
	const OTHER_AUDIENCE: &str = "https://other.example.test";
	const REQUEST_TOKEN_PATH: &str =
		"/_matrix/client/v3/user/%40alice%3Alocalhost/openid/request_token";
	const STORED_TOKENS: &str = "openidtoken_expiresatuserid";

	#[test]
	fn openid_tokens_bind_to_the_requested_audience() -> Result {
		let harness = Harness::new("mindroom_openid_audience", [])?;

		harness.with_services(async |services| {
			services
				.users
				.create(user_id!("@alice:localhost"), Some("password"), None)
				.await?;
			services
				.users
				.create_device(
					user_id!("@alice:localhost"),
					Some(device_id!("OPENID")),
					(Some(ACCESS_TOKEN), None),
					None,
					None,
					None,
				)
				.await?;

			let (state, _guard) = tuwunel_api::router::state::create(services.clone());
			let router =
				tuwunel_api::router::build(Router::new(), &services.server).with_state(state);

			advertises_the_capability(&router).await;
			verifies_per_audience_table(&router).await;
			rejects_invalid_audiences(&router).await;
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
		let query = form_urlencoded::Serializer::new(String::new())
			.append_pair("access_token", &unbound)
			.append_pair("io.mindroom.audience", AUDIENCE)
			.append_pair("io.mindroom.audience", AUDIENCE)
			.finish();
		let (status, body) = send(router, get(&userinfo_uri(&query))).await;
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

	async fn request_token(router: &Router, body: Option<Value>) -> String {
		let (status, body) = send(router, post(REQUEST_TOKEN_PATH, body)).await;
		assert_eq!(status, StatusCode::OK, "request_token: {body}");
		assert_eq!(body["token_type"], "Bearer", "request_token: {body}");
		body["access_token"]
			.as_str()
			.expect("request_token returns an access token")
			.to_owned()
	}

	async fn assert_userinfo(
		router: &Router,
		token: &str,
		audience: Option<&str>,
		expected: StatusCode,
	) {
		let mut query = form_urlencoded::Serializer::new(String::new());
		query.append_pair("access_token", token);
		if let Some(audience) = audience {
			query.append_pair("io.mindroom.audience", audience);
		}

		let (status, body) = send(router, get(&userinfo_uri(&query.finish()))).await;
		assert_eq!(status, expected, "userinfo with audience {audience:?}: {body}");
		if expected == StatusCode::OK {
			assert_eq!(body["sub"], USER, "userinfo subject: {body}");
		} else {
			assert_eq!(body["errcode"], "M_UNAUTHORIZED", "userinfo error: {body}");
		}
	}

	fn userinfo_uri(query: &str) -> String {
		format!("/_matrix/federation/v1/openid/userinfo?{query}")
	}

	fn get(uri: &str) -> Request<Body> {
		Request::builder()
			.method("GET")
			.uri(uri)
			.header("X-Forwarded-For", "127.0.0.1")
			.body(Body::empty())
			.expect("valid request")
	}

	fn post(uri: &str, body: Option<Value>) -> Request<Body> {
		let body = body.map_or_else(Body::empty, |body| Body::from(body.to_string()));

		Request::builder()
			.method("POST")
			.uri(uri)
			.header(header::AUTHORIZATION, format!("Bearer {ACCESS_TOKEN}"))
			.header(header::CONTENT_TYPE, "application/json")
			.header("X-Forwarded-For", "127.0.0.1")
			.body(body)
			.expect("valid request")
	}

	async fn send(router: &Router, request: Request<Body>) -> (StatusCode, Value) {
		let response = router
			.clone()
			.oneshot(request)
			.await
			.expect("router response");
		let status = response.status();
		let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
			.await
			.expect("readable response body");
		let body = serde_json::from_slice(&bytes).expect("JSON response body");

		(status, body)
	}
}
