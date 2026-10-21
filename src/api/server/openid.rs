use axum::{Json, extract::State};
use http::Uri;
use ruma::{OwnedRoomId, api::federation::openid::get_openid_userinfo};
use serde::Deserialize;
use serde_json::{Value, json};
use tuwunel_core::{Err, Result, err};

use crate::Ruma;

/// Query parameters this fork reads beside ruma's `access_token`.
#[derive(Deserialize)]
struct AudienceQuery {
	/// Relying party the token must be bound to (`io.mindroom.openid_audience`).
	#[serde(rename = "io.mindroom.audience")]
	audience: Option<String>,
}

/// Room the token's user must be joined to.
#[derive(Deserialize)]
struct RoomQuery {
	#[serde(rename = "io.mindroom.room_id")]
	room_id: Option<OwnedRoomId>,
}

/// # `GET /_matrix/federation/v1/openid/userinfo`
///
/// Get information about the user that generated the OpenID token.
///
/// - With `mindroom_openid_audience` and `io.mindroom.audience`, only a token
///   bound to that audience verifies; without the parameter, only an unbound
///   token does. Without `mindroom_openid_audience` the parameter is ignored.
/// - With `io.mindroom.room_id`, the token verifies only when its user is
///   joined to that room, and the response says so with
///   `"io.mindroom.room_joined": true`, which a server that ignores the
///   parameter never sends.
pub(crate) async fn get_openid_userinfo_route(
	State(services): State<crate::State>,
	uri: Uri,
	body: Ruma<get_openid_userinfo::v1::Request>,
) -> Result<Json<Value>> {
	let query = uri.query().unwrap_or_default();
	let audience = if services.config.mindroom_openid_audience {
		let AudienceQuery { audience } = serde_html_form::from_str(query).map_err(|_| {
			err!(Request(InvalidParam("Invalid `io.mindroom.audience` parameter")))
		})?;
		audience
	} else {
		None
	};

	let RoomQuery { room_id } = serde_html_form::from_str(query)
		.map_err(|_| err!(Request(InvalidParam("Invalid `io.mindroom.room_id` parameter"))))?;

	let sub = services
		.users
		.find_from_openid_token(&body.access_token, audience.as_deref())
		.await?;

	let Some(room_id) = room_id else {
		return Ok(Json(json!({ "sub": sub })));
	};

	if !services
		.state_cache
		.is_joined(&sub, &room_id)
		.await
	{
		return Err!(Request(Forbidden("The token's user is not joined to the room")));
	}

	Ok(Json(json!({ "sub": sub, "io.mindroom.room_joined": true })))
}
