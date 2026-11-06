use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// Selection of userinfo response claims.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct UserInfo {
	/// Unique identifier. Usually a number on most services; for github it is
	/// the account `id`. We consider a concatenation of the `iss` and `sub` to
	/// be a universally unique identifier for some user/identity; we index that
	/// in `oauthidpsub_oauthid`.
	///
	/// Considered for user mxid only if none of the better fields are defined.
	pub sub: String,

	/// The login username we first consider when defined.
	pub preferred_username: Option<String>,

	/// The login username considered.
	pub username: Option<String>,

	/// The login username considered if none preferred.
	pub nickname: Option<String>,

	/// The login username (github). GitHub releases it for anyone to register
	/// after a rename, so it never identifies the account.
	pub login: Option<String>,

	/// Full name.
	pub name: Option<String>,

	/// First name.
	pub given_name: Option<String>,

	/// Last name.
	pub family_name: Option<String>,

	/// Email address (`email` scope).
	pub email: Option<String>,

	/// Whether the provider verified `email` (read by `email_verified` below): any
	/// value it sends other than `true` reads as unverified, and only an absent
	/// or null claim as unknown.
	#[serde(
		default,
		deserialize_with = "lenient_bool",
		skip_serializing_if = "Option::is_none"
	)]
	pub email_verified: Option<bool>,

	/// URL to pfp (github/gitlab)
	pub avatar_url: Option<String>,

	/// URL to pfp (google)
	pub picture: Option<String>,
}

fn lenient_bool<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<bool>, D::Error> {
	Ok(Option::<Value>::deserialize(deserializer)?.map(|value| email_verified(&value)))
}

/// Whether an `email_verified` claim a provider sent says verified: `true`, or
/// `"true"` in any case, as some providers send it. Any other value does not.
#[must_use]
pub fn email_verified(value: &Value) -> bool {
	match value {
		| Value::Bool(verified) => *verified,
		| Value::String(verified) => verified.eq_ignore_ascii_case("true"),
		| _ => false,
	}
}
