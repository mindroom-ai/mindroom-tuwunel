//! Shared helpers for the user directory tests: three local accounts with
//! access tokens, the real client router, and a search for their prefix.

use std::sync::Arc;

use axum::{Router, body::Body};
use serde_json::{Value, json};
use tower::ServiceExt;
use tuwunel_api::router::state::Guard;
use tuwunel_core::{
	Result,
	http::{Method, Request, StatusCode, header},
	ruma::user_id,
};
use tuwunel_service::Services;

use super::api_router;

pub(crate) const SEARCHER_TOKEN: &str = "directory-deactivated-searcher-token-0123456789";
pub(crate) const ACTIVE_TOKEN: &str = "directory-deactivated-active-token-0123456789";
pub(crate) const GONE_TOKEN: &str = "directory-deactivated-gone-token-0123456789";

pub(crate) const ACTIVE: &str = "@dir_active:localhost";
pub(crate) const GONE: &str = "@dir_gone:localhost";

/// Creates the searcher and two local accounts with access tokens, and
/// returns the real client router with the guard that must outlive it.
pub(crate) async fn setup(services: &Arc<Services>) -> Result<(Router, Guard)> {
	for (user, token) in [
		(user_id!("@dir_searcher:localhost"), SEARCHER_TOKEN),
		(user_id!("@dir_active:localhost"), ACTIVE_TOKEN),
		(user_id!("@dir_gone:localhost"), GONE_TOKEN),
	] {
		services
			.users
			.create(user, Some("password"), None)
			.await?;
		services
			.users
			.create_device(user, None, (Some(token), None), None, None, None)
			.await?;
	}

	let (state, guard) = tuwunel_api::router::state::create(services.clone());
	let router = api_router(&services.server).with_state(state);

	Ok((router, guard))
}

/// Searches for `dir_` as the searcher and returns the sorted user IDs.
pub(crate) async fn search(router: &Router) -> Vec<String> {
	let body = json!({"search_term": "dir_", "limit": 100});
	let body =
		request(router, Method::POST, "/user_directory/search", SEARCHER_TOKEN, body).await;
	assert_eq!(body["limited"], false, "search: {body}");

	let mut users: Vec<_> = body["results"]
		.as_array()
		.expect("search returns results")
		.iter()
		.map(|user| {
			user["user_id"]
				.as_str()
				.expect("each result has a user ID")
				.to_owned()
		})
		.collect();

	users.sort_unstable();
	users
}

pub(crate) async fn request(
	router: &Router,
	method: Method,
	path: &str,
	token: &str,
	body: Value,
) -> Value {
	let request = Request::builder()
		.method(method)
		.uri(format!("/_matrix/client/v3{path}"))
		.header(header::AUTHORIZATION, format!("Bearer {token}"))
		.header(header::CONTENT_TYPE, "application/json")
		.header("X-Forwarded-For", "127.0.0.1")
		.body(Body::from(body.to_string()))
		.expect("valid client request");
	let response = router
		.clone()
		.oneshot(request)
		.await
		.expect("client response");
	let status = response.status();
	let body = axum::body::to_bytes(response.into_body(), 1 << 20)
		.await
		.expect("read response");
	let body: Value = serde_json::from_slice(&body).expect("JSON response");
	assert_eq!(status, StatusCode::OK, "{path}: {body}");
	body
}
