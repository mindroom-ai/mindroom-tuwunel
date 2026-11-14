use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tuwunel_core::utils::deserialize_lenient_bool;

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

	/// Whether the provider verified `email`, false for any claim other than a
	/// boolean or its string spelling.
	#[serde(default, deserialize_with = "deserialize_lenient_bool")]
	pub email_verified: Option<bool>,

	/// URL to pfp (github/gitlab)
	pub avatar_url: Option<String>,

	/// URL to pfp (google)
	pub picture: Option<String>,

	/// Google's hosted-domain claim: the Workspace domain of the account,
	/// absent for consumer accounts; any claim that is not a string is absent.
	#[serde(default, deserialize_with = "lenient_string")]
	pub hd: Option<String>,
}

fn lenient_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
	D: serde::Deserializer<'de>,
{
	Ok(match serde_json::Value::deserialize(deserializer)? {
		| serde_json::Value::String(string) => Some(string),
		| _ => None,
	})
}

impl UserInfo {
	/// The email address the provider vouches for.
	///
	/// An `email_verified` claim decides when present. Without one, only a
	/// trusted provider vouches for the address it reports.
	#[inline]
	#[must_use]
	pub fn verified_email(&self, trusted: bool) -> Option<&str> {
		self.email
			.as_deref()
			.filter(|_| self.email_verified.unwrap_or(trusted))
	}

	/// Whether every claim of a registration rule, or of a provider's
	/// `assigned_accounts_claims`, holds. An empty set and an unknown claim
	/// name never hold here; an empty `assigned_accounts_claims` requires
	/// nothing, so its caller treats it as met without asking. `email` counts
	/// only when the provider vouches for it, and `email` and `hd` ignore ASCII
	/// case; the others are exact.
	#[must_use]
	pub fn meets(&self, claims: &BTreeMap<String, String>, trusted: bool) -> bool {
		let exact = |have: Option<&str>, want: &str| have == Some(want);
		let folded = |have: Option<&str>, want: &str| {
			have.is_some_and(|have| have.eq_ignore_ascii_case(want))
		};

		!claims.is_empty()
			&& claims
				.iter()
				.all(|(claim, want)| match claim.as_str() {
					| "sub" => exact(Some(&self.sub), want),
					| "preferred_username" => exact(self.preferred_username.as_deref(), want),
					| "username" => exact(self.username.as_deref(), want),
					| "nickname" => exact(self.nickname.as_deref(), want),
					| "login" => exact(self.login.as_deref(), want),
					| "name" => exact(self.name.as_deref(), want),
					| "given_name" => exact(self.given_name.as_deref(), want),
					| "family_name" => exact(self.family_name.as_deref(), want),
					| "email" => folded(self.verified_email(trusted), want),
					| "hd" => folded(self.hd.as_deref(), want),
					| _ => false,
				})
	}
}

#[cfg(test)]
mod tests {
	use std::collections::BTreeMap;

	use super::UserInfo;

	fn claims(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
		pairs
			.iter()
			.map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
			.collect()
	}

	#[test]
	fn non_string_hd_is_absent() {
		let user: UserInfo = serde_json::from_str(r#"{"sub": "1", "hd": true}"#).unwrap();

		assert_eq!(user.hd, None);

		let user: UserInfo =
			serde_json::from_str(r#"{"sub": "1", "hd": "example.com"}"#).unwrap();

		assert_eq!(user.hd.as_deref(), Some("example.com"));
	}

	#[test]
	fn meets_hd_ignoring_case() {
		let user = UserInfo {
			hd: Some("EXAMPLE.com".into()),
			..Default::default()
		};

		assert!(user.meets(&claims(&[("hd", "example.com")]), false));
		assert!(!user.meets(&claims(&[("hd", "other.example")]), false));
		assert!(!UserInfo::default().meets(&claims(&[("hd", "example.com")]), false));
	}

	#[test]
	fn meets_email_only_when_verified() {
		let rule = claims(&[("email", "alice@school.example")]);
		let user = |email_verified| UserInfo {
			email: Some("Alice@School.example".into()),
			email_verified,
			..Default::default()
		};

		assert!(user(Some(true)).meets(&rule, false));
		assert!(!user(Some(false)).meets(&rule, false));
		assert!(!user(Some(false)).meets(&rule, true));
		assert!(!user(None).meets(&rule, false));
		assert!(user(None).meets(&rule, true));
	}

	#[test]
	fn meets_never_matches_empty_claims() {
		let user = UserInfo { sub: "1".into(), ..Default::default() };

		assert!(!user.meets(&BTreeMap::new(), true));
	}

	#[test]
	fn meets_needs_every_claim() {
		let rule = claims(&[("hd", "example.com"), ("email", "alice@example.com")]);
		let user = UserInfo {
			hd: Some("example.com".into()),
			email: Some("alice@example.com".into()),
			email_verified: Some(true),
			..Default::default()
		};

		assert!(user.meets(&rule, false));
		assert!(!UserInfo { hd: None, ..user.clone() }.meets(&rule, false));
		assert!(!UserInfo { email_verified: Some(false), ..user }.meets(&rule, false));
	}

	#[test]
	fn meets_other_claims_exactly() {
		let user = UserInfo {
			sub: "42".into(),
			name: Some("Alice".into()),
			..Default::default()
		};

		assert!(user.meets(&claims(&[("name", "Alice")]), false));
		assert!(!user.meets(&claims(&[("name", "alice")]), false));
		assert!(user.meets(&claims(&[("sub", "42")]), false));
	}

	#[test]
	fn meets_fails_on_unknown_claim() {
		let user = UserInfo { sub: "1".into(), ..Default::default() };

		assert!(!user.meets(&claims(&[("groups", "x")]), false));
		assert!(!user.meets(&claims(&[("sub", "1"), ("groups", "x")]), false));
	}
}
