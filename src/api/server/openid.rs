use axum::extract::State;
use http::Uri;
use ruma::api::federation::openid::get_openid_userinfo;
use serde::Deserialize;
use tuwunel_core::{Result, err};

use crate::Ruma;

/// Query parameters this fork reads beside ruma's `access_token`.
#[derive(Deserialize)]
struct AudienceQuery {
	/// Relying party the token must be bound to (`io.mindroom.openid_audience`).
	#[serde(rename = "io.mindroom.audience")]
	audience: Option<String>,
}

/// # `GET /_matrix/federation/v1/openid/userinfo`
///
/// Get information about the user that generated the OpenID token.
///
/// - With `io.mindroom.audience`, only a token bound to that audience verifies;
///   without it, only an unbound token does
pub(crate) async fn get_openid_userinfo_route(
	State(services): State<crate::State>,
	uri: Uri,
	body: Ruma<get_openid_userinfo::v1::Request>,
) -> Result<get_openid_userinfo::v1::Response> {
	let query = uri.query().unwrap_or_default();
	let AudienceQuery { audience } = serde_html_form::from_str(query)
		.map_err(|_| err!(Request(InvalidParam("Invalid `io.mindroom.audience` parameter"))))?;

	Ok(get_openid_userinfo::v1::Response::new(
		services
			.users
			.find_from_openid_token(&body.access_token, audience.as_deref())
			.await?,
	))
}
