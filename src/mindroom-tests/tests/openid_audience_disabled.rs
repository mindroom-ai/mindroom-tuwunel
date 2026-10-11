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
		openid::{USER, assert_userinfo, get, request_token, send, setup, userinfo_uri},
	};

	const AUDIENCE: &str = "https://mindroom.example.test";
	const OTHER_AUDIENCE: &str = "https://other.example.test";
	const STORED_TOKENS: &str = "openidtoken_expiresatuserid";

	/// Without `mindroom_openid_audience` the server behaves as upstream.
	#[test]
	fn openid_audiences_are_ignored_by_default() -> Result {
		let harness = Harness::new("mindroom_openid_audience_disabled", [])?;

		harness.with_services(async |services| {
			let (router, _guard) = setup(&services).await?;

			does_not_advertise_the_capability(&router).await;
			ignores_the_body_audience(&router).await;
			ignores_the_query_audience(&router).await;
			verifies_stored_bound_tokens_as_unbound(&services, &router).await;

			Ok(())
		})
	}

	async fn does_not_advertise_the_capability(router: &Router) {
		let (status, body) = send(router, get("/_matrix/client/versions")).await;
		assert_eq!(status, StatusCode::OK, "versions: {body}");
		assert_eq!(
			body["unstable_features"]["io.mindroom.openid_audience"],
			Value::Null,
			"versions must not advertise audience-bound OpenID tokens: {body}",
		);
	}

	async fn ignores_the_body_audience(router: &Router) {
		let token = request_token(router, Some(json!({"io.mindroom.audience": AUDIENCE}))).await;
		assert_userinfo(router, &token, None, StatusCode::OK).await;

		// Values the option would reject are not validated either.
		let token = request_token(router, Some(json!({"io.mindroom.audience": 1}))).await;
		assert_userinfo(router, &token, None, StatusCode::OK).await;

		// A body that is not canonical JSON still gets an unbound token.
		let token =
			request_token(router, Some(json!({"io.mindroom.audience": AUDIENCE, "extra": 1.5})))
				.await;
		assert_userinfo(router, &token, None, StatusCode::OK).await;
	}

	async fn ignores_the_query_audience(router: &Router) {
		let token = request_token(router, Some(json!({}))).await;
		assert_userinfo(router, &token, Some(AUDIENCE), StatusCode::OK).await;

		let token = request_token(router, Some(json!({"io.mindroom.audience": AUDIENCE}))).await;
		assert_userinfo(router, &token, Some(OTHER_AUDIENCE), StatusCode::OK).await;

		// A repeated audience is not parsed at all.
		let token = request_token(router, None).await;
		let uri = userinfo_uri(&[
			("access_token", &token),
			("io.mindroom.audience", AUDIENCE),
			("io.mindroom.audience", AUDIENCE),
		]);
		let (status, body) = send(router, get(&uri)).await;
		assert_eq!(status, StatusCode::OK, "repeated audience: {body}");
		assert_eq!(body["sub"], USER, "repeated audience: {body}");
	}

	async fn verifies_stored_bound_tokens_as_unbound(services: &Arc<Services>, router: &Router) {
		// A row bound while the option was on.
		let mut bound = millis_since_unix_epoch()
			.saturating_add(60_000)
			.to_be_bytes()
			.to_vec();
		bound.extend_from_slice(USER.as_bytes());
		bound.push(0xFF);
		bound.extend_from_slice(AUDIENCE.as_bytes());
		services.db[STORED_TOKENS].insert("bound-openid-token", bound);

		assert_eq!(
			services
				.users
				.find_from_openid_token("bound-openid-token", None)
				.await
				.expect("a stored bound token verifies without an audience"),
			user_id!("@alice:localhost"),
		);
		assert_userinfo(router, "bound-openid-token", None, StatusCode::OK).await;
	}
}
