mod support;

#[cfg(test)]
mod tests {
	use axum::{Router, body::Body};
	use serde_json::{Value, json};
	use tower::ServiceExt;
	use tuwunel_core::{
		Result,
		http::{Request, StatusCode, header},
		jwt::{EncodingKey, Header, encode},
		ruma::{UserId, device_id},
	};
	use tuwunel_service::{Services, oauth::Session, users::PASSWORD_SENTINEL};

	use super::support::Harness;

	const IDP: &str = "uiaa-policy-idp";
	const JWT_SECRET: &str = "uiaa-sso-policy-jwt-secret";

	/// Pin the UIAA flows the fork advertises on top of upstream's
	/// credential-matched password flow.
	///
	/// Upstream offers `m.login.password` exactly when the account holds a
	/// real password (or is an LDAP account under LDAP). The fork adds that
	/// `m.login.sso` is offered only to SSO-origin accounts, never to a
	/// password account whose device came from an identity provider, that JWT
	/// is never advertised (its fallback page is not implemented) and is
	/// refused for SSO-origin accounts, and that a legacy SSO account whose
	/// origin was rewritten to `password` is repaired before the flows are
	/// chosen.
	#[test]
	fn uiaa_flows_follow_account_origin_and_credentials() -> Result {
		let harness = Harness::new("mindroom_uiaa_sso_policy", [
			format!("identity_provider.test.client_id=\"{IDP}\""),
			"identity_provider.test.client_secret=\"test-secret\"".to_owned(),
			"identity_provider.test.brand=\"test\"".to_owned(),
			"identity_provider.test.issuer_url=\"https://idp.invalid\"".to_owned(),
			"jwt.enable=true".to_owned(),
			format!("jwt.key=\"{JWT_SECRET}\""),
			"jwt.format=\"HMAC\"".to_owned(),
			"jwt.algorithm=\"HS256\"".to_owned(),
			"jwt.register_user=false".to_owned(),
		])?;

		harness.with_services(async |services| {
			let (state, _guard) = tuwunel_api::router::state::create(services.clone());
			let router =
				tuwunel_api::router::build(Router::new(), &services.server).with_state(state);

			let password = json!([{"stages": ["m.login.password"]}]);
			let sso = json!([{"stages": ["m.login.sso"]}]);
			let password_and_sso = json!([
				{"stages": ["m.login.password"]},
				{"stages": ["m.login.sso"]},
			]);

			// A password account: password only, and JWT is not advertised even
			// though JWT login is enabled.
			let local = account(&services, "local", "test-password", "password").await?;
			let challenge = uiaa_challenge(&router, &local.1).await;
			assert_eq!(challenge["flows"], password, "password account: {challenge}");
			assert_no_sso_binding(&challenge, "password account");

			// A passwordless SSO account: SSO only, bound to the single provider.
			let sso_user = account(&services, "sso", PASSWORD_SENTINEL, "sso").await?;
			let challenge = uiaa_challenge(&router, &sso_user.1).await;
			assert_eq!(challenge["flows"], sso, "passwordless SSO account: {challenge}");
			assert_sso_binding(&challenge, "passwordless SSO account");

			// An SSO-origin account that also holds a real password keeps both,
			// as upstream decides from the stored credential. Setting a real
			// password rewrites the origin to `password`, so restore `sso`.
			let sso_local = account(&services, "sso-local", "test-password", "sso").await?;
			services.users.set_origin(&sso_local.0, "sso");
			let challenge = uiaa_challenge(&router, &sso_local.1).await;
			assert_eq!(
				challenge["flows"], password_and_sso,
				"SSO account with a real password: {challenge}"
			);
			assert_sso_binding(&challenge, "SSO account with a real password");

			// A password account signed in on a device an identity provider
			// issued is still not offered SSO.
			let oidc_device =
				account(&services, "oidc-device", "test-password", "password").await?;
			services
				.users
				.mark_oidc_device(&oidc_device.0, device_id!("UIAADEVICE"), IDP);
			let challenge = uiaa_challenge(&router, &oidc_device.1).await;
			assert_eq!(
				challenge["flows"], password,
				"password account on an identity-provider device: {challenge}"
			);
			assert_no_sso_binding(&challenge, "password account on an identity-provider device");

			// A legacy SSO account: the sentinel password with a `password`
			// origin and a linked provider session. UIAA repairs the origin
			// first, so it is challenged as the SSO account it is.
			let legacy = account(&services, "legacy", PASSWORD_SENTINEL, "password").await?;
			services
				.oauth
				.sessions
				.put(&Session {
					idp_id: Some(IDP.to_owned()),
					sess_id: Some("uiaa-policy-legacy-session".to_owned()),
					user_id: Some(legacy.0.clone()),
					..Default::default()
				})
				.await;
			let challenge = uiaa_challenge(&router, &legacy.1).await;
			assert_eq!(challenge["flows"], sso, "legacy SSO account: {challenge}");
			assert_sso_binding(&challenge, "legacy SSO account");
			assert_eq!(services.users.origin(&legacy.0).await?, "sso");

			// A passwordless account without an identity provider has no flow.
			let passwordless =
				account(&services, "passwordless", PASSWORD_SENTINEL, "password").await?;
			let challenge = uiaa_challenge(&router, &passwordless.1).await;
			assert_eq!(challenge["flows"], json!([]), "passwordless account: {challenge}");

			// JWT UIAA is refused for an SSO-origin account, so a session of an
			// SSO account cannot be deactivated through a JWT stage.
			let session = uiaa_challenge(&router, &sso_user.1).await["session"].clone();
			let token = encode(
				&Header::default(),
				&json!({"sub": sso_user.0.localpart()}),
				&EncodingKey::from_secret(JWT_SECRET.as_bytes()),
			)
			.expect("mint a JWT");
			let (status, body) = deactivate(
				&router,
				&sso_user.1,
				json!({"auth": {"type": "org.matrix.login.jwt", "token": token, "session": session}}),
			)
			.await;
			assert_eq!(status, StatusCode::FORBIDDEN, "JWT stage for an SSO account: {body}");
			assert_eq!(body["errcode"], "M_FORBIDDEN", "JWT stage for an SSO account: {body}");
			assert!(
				!services.users.is_deactivated(&sso_user.0).await?,
				"a refused JWT stage must not deactivate the account",
			);

			Ok(())
		})
	}

	/// Create an account with the given credential and origin and sign it in,
	/// returning the user ID and the access token.
	async fn account(
		services: &Services,
		localpart: &str,
		credential: &str,
		origin: &str,
	) -> Result<(tuwunel_core::ruma::OwnedUserId, String)> {
		let user_id = UserId::parse_with_server_name(localpart, services.globals.server_name())?;
		services
			.users
			.create(&user_id, Some(credential), Some(origin))
			.await?;

		let token = format!("uiaa-sso-policy-access-token-{localpart}");
		services
			.users
			.create_device(
				&user_id,
				Some(device_id!("UIAADEVICE")),
				(Some(&token), None),
				None,
				None,
				None,
			)
			.await?;

		Ok((user_id, token))
	}

	async fn uiaa_challenge(router: &Router, token: &str) -> Value {
		let (status, body) = deactivate(router, token, json!({})).await;
		assert_eq!(status, StatusCode::UNAUTHORIZED, "expected a UIAA challenge: {body}");
		assert!(
			body["session"]
				.as_str()
				.is_some_and(|session| !session.is_empty()),
			"the UIAA challenge omitted its session: {body}",
		);

		body
	}

	fn assert_sso_binding(challenge: &Value, context: &str) {
		assert_eq!(
			challenge["params"]["m.login.sso"]["identity_providers"],
			json!([{"id": IDP}]),
			"{context}: SSO challenge lost its provider binding: {challenge}",
		);
	}

	fn assert_no_sso_binding(challenge: &Value, context: &str) {
		assert!(
			challenge["params"]["m.login.sso"].is_null(),
			"{context}: unexpected SSO provider binding: {challenge}",
		);
	}

	async fn deactivate(router: &Router, token: &str, body: Value) -> (StatusCode, Value) {
		let request = Request::builder()
			.method("POST")
			.uri("/_matrix/client/v3/account/deactivate")
			.header(header::AUTHORIZATION, format!("Bearer {token}"))
			.header(header::CONTENT_TYPE, "application/json")
			.header("X-Forwarded-For", "127.0.0.1")
			.body(Body::from(body.to_string()))
			.expect("valid request");

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
