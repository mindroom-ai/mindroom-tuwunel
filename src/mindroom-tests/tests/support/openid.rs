//! Shared helpers for the OpenID audience tests.

use std::sync::Arc;

use axum::{Router, body::Body};
use serde_json::Value;
use tower::ServiceExt;
use tuwunel_api::router::state::Guard;
use tuwunel_core::{
	Result,
	http::{Request, StatusCode, header},
	ruma::{device_id, user_id},
};
use tuwunel_service::Services;
use url::form_urlencoded;

pub(crate) const USER: &str = "@alice:localhost";
pub(crate) const ACCESS_TOKEN: &str = "openid-audience-access-token-0123456789";
pub(crate) const REQUEST_TOKEN_PATH: &str =
	"/_matrix/client/v3/user/%40alice%3Alocalhost/openid/request_token";

/// Creates the test user with a device holding `ACCESS_TOKEN` and returns the
/// real router with the guard that must outlive it.
pub(crate) async fn setup(services: &Arc<Services>) -> Result<(Router, Guard)> {
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

	let (state, guard) = tuwunel_api::router::state::create(services.clone());
	let router = tuwunel_api::router::build(Router::new(), &services.server).with_state(state);

	Ok((router, guard))
}

pub(crate) async fn request_token(router: &Router, body: Option<Value>) -> String {
	let (status, body) = send(router, post(REQUEST_TOKEN_PATH, body)).await;
	assert_eq!(status, StatusCode::OK, "request_token: {body}");
	assert_eq!(body["token_type"], "Bearer", "request_token: {body}");
	body["access_token"]
		.as_str()
		.expect("request_token returns an access token")
		.to_owned()
}

pub(crate) async fn assert_userinfo(
	router: &Router,
	token: &str,
	audience: Option<&str>,
	expected: StatusCode,
) {
	let mut pairs = vec![("access_token", token)];
	pairs.extend(audience.map(|audience| ("io.mindroom.audience", audience)));

	let (status, body) = send(router, get(&userinfo_uri(&pairs))).await;
	assert_eq!(status, expected, "userinfo with audience {audience:?}: {body}");
	if expected == StatusCode::OK {
		assert_eq!(body["sub"], USER, "userinfo subject: {body}");
	} else {
		assert_eq!(body["errcode"], "M_UNAUTHORIZED", "userinfo error: {body}");
	}
}

pub(crate) fn userinfo_uri(pairs: &[(&str, &str)]) -> String {
	let query = form_urlencoded::Serializer::new(String::new())
		.extend_pairs(pairs)
		.finish();

	format!("/_matrix/federation/v1/openid/userinfo?{query}")
}

pub(crate) fn get(uri: &str) -> Request<Body> {
	Request::builder()
		.method("GET")
		.uri(uri)
		.header("X-Forwarded-For", "127.0.0.1")
		.body(Body::empty())
		.expect("valid request")
}

pub(crate) fn post(uri: &str, body: Option<Value>) -> Request<Body> {
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

pub(crate) async fn send(router: &Router, request: Request<Body>) -> (StatusCode, Value) {
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
