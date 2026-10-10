use std::time::Duration;

use axum::extract::State;
use ruma::{CanonicalJsonValue, api::client::account, authentication::TokenType};
use tuwunel_core::{Err, Result, utils};

use super::TOKEN_LENGTH;
use crate::Ruma;

/// Request body field naming the relying party a token is bound to
/// (`io.mindroom.openid_audience`).
const AUDIENCE_FIELD: &str = "io.mindroom.audience";

/// Longest accepted audience, in characters.
const AUDIENCE_MAX_CHARS: usize = 255;

/// # `POST /_matrix/client/v3/user/{userId}/openid/request_token`
///
/// Request an OpenID token to verify identity with third-party services.
///
/// - The token generated is only valid for the OpenID API
/// - A token requested with `io.mindroom.audience` verifies only for a relying
///   party that presents the same audience
pub(crate) async fn create_openid_token_route(
	State(services): State<crate::State>,
	body: Ruma<account::request_openid_token::v3::Request>,
) -> Result<account::request_openid_token::v3::Response> {
	let sender_user = body.sender_user();

	if sender_user != body.user_id {
		return Err!(Request(InvalidParam(
			"Not allowed to request OpenID tokens on behalf of other users",
		)));
	}

	let audience = requested_audience(body.json_body.as_ref())?;
	let access_token = utils::random_string(TOKEN_LENGTH);
	let expires_in =
		services
			.users
			.create_openid_token(&body.user_id, &access_token, audience)?;

	Ok(account::request_openid_token::v3::Response {
		access_token,
		token_type: TokenType::Bearer,
		matrix_server_name: services.server.name.clone(),
		expires_in: Duration::from_secs(expires_in),
	})
}

/// Reads the optional audience from the request body; absent means unbound.
fn requested_audience(json_body: Option<&CanonicalJsonValue>) -> Result<Option<&str>> {
	let Some(CanonicalJsonValue::Object(body)) = json_body else {
		return Ok(None);
	};

	let Some(audience) = body.get(AUDIENCE_FIELD) else {
		return Ok(None);
	};

	let CanonicalJsonValue::String(audience) = audience else {
		return Err!(Request(InvalidParam("`io.mindroom.audience` must be a string")));
	};

	if audience.is_empty()
		|| audience.chars().count() > AUDIENCE_MAX_CHARS
		|| audience.chars().any(char::is_control)
	{
		return Err!(Request(InvalidParam(
			"`io.mindroom.audience` must be 1 to 255 characters without control characters",
		)));
	}

	Ok(Some(audience))
}
