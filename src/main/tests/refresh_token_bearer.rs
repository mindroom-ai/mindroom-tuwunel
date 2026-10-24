#![cfg(test)]

use reqwest::StatusCode;
use serde_json::{Value, json};
use tuwunel_core::{
	Err, Result,
	ruma::{UserId, device_id},
};
use tuwunel_service::{Services, users::device::generate_refresh_token};

use self::fixture::boot;

#[expect(
	dead_code,
	reason = "Only listener readiness is shared with the client API harness."
)]
mod client;

mod fixture;

/// A refresh token does not authenticate requests.
///
/// Refresh tokens share the access token map, so one presented as a bearer
/// must be refused, while `/refresh` still redeems it.
#[test]
fn refresh_token_is_not_an_access_token() -> Result {
	let options = ["argon2_m_cost=64", "argon2_t_cost=1"];

	boot("refresh-token-bearer", options, exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	let user = UserId::parse_with_server_name("bearer", services.globals.server_name())?;
	let device = device_id!("BEARERDEVICE");

	services
		.users
		.create(&user, Some("password"), None)
		.await?;

	let (access, expires_in) = services.users.generate_access_token(true);
	let refresh = generate_refresh_token();

	services
		.users
		.create_device(
			&user,
			Some(device),
			(Some(&access), expires_in),
			Some(&refresh),
			None,
			None,
		)
		.await?;

	let http = &services.client.clients.default;
	let whoami = format!("{base}/_matrix/client/v3/account/whoami");

	let refused = http
		.get(&whoami)
		.bearer_auth(&refresh)
		.send()
		.await?;

	if refused.status() != StatusCode::UNAUTHORIZED {
		return Err!("a refresh token authenticated a request: {}", refused.status());
	}

	let body: Value = refused.json().await?;
	if body["errcode"] != "M_UNKNOWN_TOKEN" {
		return Err!("a refused refresh token answered {body}");
	}

	let accepted = http
		.get(&whoami)
		.bearer_auth(&access)
		.send()
		.await?;

	if !accepted.status().is_success() {
		return Err!("the access token was refused: {}", accepted.status());
	}

	let redeemed = http
		.post(format!("{base}/_matrix/client/v3/refresh"))
		.json(&json!({ "refresh_token": refresh }))
		.send()
		.await?;

	if !redeemed.status().is_success() {
		return Err!("/refresh refused the refresh token: {}", redeemed.status());
	}

	Ok(())
}
