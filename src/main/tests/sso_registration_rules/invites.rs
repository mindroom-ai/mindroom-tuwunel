//! Registration invites: addresses an administrator lets register, each for
//! one sign-in, beside the provider's registration rules.

use reqwest::{Method, Response, StatusCode};
use serde_json::{Value, json};
use tuwunel_core::{
	Result, config::RegistrationRule, ruma::events::room::member::MembershipState,
};
use tuwunel_service::{Services, threepid::canonicalize_email, users::RegistrationInvite};

use super::{
	alias, boot,
	client::{Client, accepted},
	creator, creator_id, make_rooms, membership_of, refused, run, run_with, sign_in, signed_in,
	update_rules, user_id,
};

/// The invite routes, below the client API's unstable prefix.
const ROUTE: &str = "io.mindroom/admin/registration_invites";

/// The feature `/versions` lists while invites can be used.
const FEATURE: &str = "io.mindroom.registration_invites";

/// Leaves the creator an ordinary account until a test makes it an
/// administrator.
const NO_FIRST_ADMIN: &[&str] = &["registration.grant_admin_to_first_user=false"];

/// Store the invite of an address, as the admin API does once it has checked
/// the request.
fn invite(services: &Services, email: &str, rooms: &[&str], external: bool) -> Result {
	let invite = RegistrationInvite {
		rooms: rooms.iter().copied().map(alias).collect(),
		external,
	};

	services
		.users
		.set_registration_invite(&canonicalize_email(email)?, &invite);

	Ok(())
}

/// Whether the address still has an invite.
async fn pending(services: &Services, email: &str) -> Result<bool> {
	Ok(services
		.users
		.registration_invite(email)
		.await?
		.is_some())
}

/// A verified address no rule lists is refused until it is invited, in
/// whatever case; its account is then external as the invite says and joined
/// to the invite's room, the invite is used up, and the account signs in again
/// without one.
#[test]
fn invite_admits_once() -> Result {
	run("sso-registration-invites-admits", async |services, base| {
		let rooms = make_rooms(services, base).await?;

		refused(services, base, "other").await?;
		invite(services, "OTHER@school.example", &["#class:localhost"], true)?;

		let response = sign_in(base, "other").await?;
		let other = signed_in(services, &response).await?;

		assert_eq!(other, user_id(services, "other")?);
		assert!(services.users.is_external(&other).await, "the invite makes it external");
		assert_eq!(
			membership_of(services, &other, &rooms.class).await,
			Some(MembershipState::Join)
		);
		assert!(!pending(services, "other@school.example").await?, "the invite is used up");

		let response = sign_in(base, "other").await?;

		assert_eq!(signed_in(services, &response).await?, other);

		Ok(())
	})
}

/// An invite that was removed admits no one.
#[test]
fn removed_invite_refuses() -> Result {
	run("sso-registration-invites-removed", async |services, base| {
		make_rooms(services, base).await?;
		invite(services, "other@school.example", &[], false)?;

		let users = &services.users;

		assert!(
			users
				.remove_registration_invite("other@school.example")
				.await,
			"there was an invite"
		);
		assert!(
			!users
				.remove_registration_invite("other@school.example")
				.await,
			"it is gone"
		);

		refused(services, base, "other").await
	})
}

/// An invite admits only an address the provider vouches for, and stays for
/// the sign-in that is.
#[test]
fn unverified_address_is_not_admitted() -> Result {
	run("sso-registration-invites-unverified", async |services, base| {
		make_rooms(services, base).await?;
		invite(services, "nobody@elsewhere.example", &[], false)?;

		refused(services, base, "nobody").await?;

		assert!(pending(services, "nobody@elsewhere.example").await?, "the invite stays");

		Ok(())
	})
}

/// An account that meets a rule and has an invite gets both: the rule's room
/// and the invite's, and it is external because the invite says so. The
/// provider reports the address in another case than the invite is stored in.
#[test]
fn invite_adds_to_a_met_rule() -> Result {
	run("sso-registration-invites-adds", async |services, base| {
		let rooms = make_rooms(services, base).await?;

		update_rules(services, |rules| {
			rules.push(RegistrationRule {
				claims: [("hd".to_owned(), "visit.example".to_owned())].into(),
				rooms: vec![alias("#open:localhost")],
				external: false,
			});
		})?;

		invite(services, "vera@visit.example", &["#class:localhost", "#open:localhost"], true)?;

		let response = sign_in(base, "vera").await?;
		let vera = signed_in(services, &response).await?;

		assert!(services.users.is_external(&vera).await, "the invite makes it external");

		for room_id in [&rooms.open, &rooms.class] {
			assert_eq!(
				membership_of(services, &vera, room_id).await,
				Some(MembershipState::Join),
				"{room_id}"
			);
		}

		assert!(!pending(services, "vera@visit.example").await?, "the invite is used up");

		Ok(())
	})
}

/// Invites do not close a provider without rules: an address without one
/// still registers, as a normal account, and the invited one gets its
/// invite's terms.
#[test]
fn open_provider_stays_open() -> Result {
	run("sso-registration-invites-open", async |services, base| {
		let rooms = make_rooms(services, base).await?;

		update_rules(services, Vec::clear)?;
		invite(services, "other@school.example", &["#class:localhost"], true)?;

		let response = sign_in(base, "olga").await?;
		let olga = signed_in(services, &response).await?;

		assert!(!services.users.is_external(&olga).await, "olga is not external");

		let response = sign_in(base, "other").await?;
		let other = signed_in(services, &response).await?;

		assert!(services.users.is_external(&other).await, "the invited account is external");
		assert_eq!(
			membership_of(services, &other, &rooms.class).await,
			Some(MembershipState::Join)
		);

		Ok(())
	})
}

/// An invite naming a room that cannot be joined still admits its address:
/// the sign-in succeeds and the invite is used up.
#[test]
fn unjoinable_room_does_not_fail_the_sign_in() -> Result {
	run("sso-registration-invites-unjoinable", async |services, base| {
		make_rooms(services, base).await?;
		invite(services, "other@school.example", &["#nowhere:localhost"], false)?;

		let response = sign_in(base, "other").await?;
		let other = signed_in(services, &response).await?;

		assert_eq!(other, user_id(services, "other")?);
		assert!(!pending(services, "other@school.example").await?, "the invite is used up");

		Ok(())
	})
}

/// An invite that cannot be read fails the sign-in and creates no account,
/// on a provider that admits the address without one: a storage fault never
/// registers an invited external person as a normal account.
#[test]
fn unreadable_invite_fails_the_sign_in() -> Result {
	run("sso-registration-invites-unreadable", async |services, base| {
		make_rooms(services, base).await?;
		update_rules(services, Vec::clear)?;

		services.db["registrationemail_invite"].insert("other@school.example", "not-json");

		let response = sign_in(base, "other").await?;

		assert!(signed_in(services, &response).await.is_err(), "the sign-in fails");
		assert!(
			!services
				.users
				.exists(&user_id(services, "other")?)
				.await,
			"no account is created"
		);

		Ok(())
	})
}

/// An account that exists already is not changed by an invite for its
/// address, and leaves the invite where it is.
#[test]
fn existing_account_keeps_its_terms() -> Result {
	run("sso-registration-invites-existing", async |services, base| {
		let rooms = make_rooms(services, base).await?;

		update_rules(services, Vec::clear)?;

		let response = sign_in(base, "other").await?;
		let other = signed_in(services, &response).await?;

		invite(services, "other@school.example", &["#class:localhost"], true)?;

		let response = sign_in(base, "other").await?;

		assert_eq!(signed_in(services, &response).await?, other);
		assert!(!services.users.is_external(&other).await, "the account is not made external");
		assert_eq!(membership_of(services, &other, &rooms.class).await, None);
		assert!(pending(services, "other@school.example").await?, "the invite stays");

		Ok(())
	})
}

/// Call an invite route as the client: the list, or the entry of an address,
/// which is sent as it is given.
async fn call(
	client: &Client<'_>,
	method: Method,
	email: Option<&str>,
	body: &Value,
) -> Result<Response> {
	let rest = email.map_or_else(|| ROUTE.to_owned(), |email| format!("{ROUTE}/{email}"));

	client
		.request(method, &client.client_url("unstable/", &rest), body)
		.await
}

/// The invites the list route answers with.
async fn listed(client: &Client<'_>) -> Result<Value> {
	let response = call(client, Method::GET, None, &json!({})).await?;

	Ok(accepted(response).await?["invites"].take())
}

/// Expect a refusal with the status and the error code, and return its
/// message.
async fn refusal(response: Response, status: StatusCode, errcode: &str) -> Result<String> {
	assert_eq!(response.status(), status, "{errcode}");

	let error: Value = response.json().await?;

	assert_eq!(error["errcode"], errcode);

	Ok(error["error"]
		.as_str()
		.unwrap_or_default()
		.to_owned())
}

/// The creator, made a server administrator.
async fn administrator<'a>(services: &'a Services, base: &'a str) -> Result<Client<'a>> {
	services
		.admin
		.make_user_admin(&creator_id(services)?)
		.await?;

	Ok(creator(services, base))
}

/// What `/versions` says of the feature.
async fn advertised(services: &Services, base: &str) -> Result<Value> {
	let versions: Value = services
		.client
		.clients
		.default
		.get(format!("{base}/_matrix/client/versions"))
		.send()
		.await?
		.json()
		.await?;

	Ok(versions["unstable_features"][FEATURE].clone())
}

/// An administrator invites an address over HTTP, in whatever case, and sees
/// it listed in its canonical form; the sign-in it admits uses it up, an
/// invite can be removed once, and a malformed address has none to remove.
#[test]
fn administrator_manages_invites_over_http() -> Result {
	run_with("sso-registration-invites-http", NO_FIRST_ADMIN, async |services, base| {
		make_rooms(services, base).await?;

		let admin = administrator(services, base).await?;
		let terms = json!({ "rooms": ["#class:localhost"], "external": true });
		let entry = json!({
			"email": "other@school.example",
			"rooms": ["#class:localhost"],
			"external": true,
		});

		let response = call(&admin, Method::PUT, Some("Other@School.example"), &terms).await?;

		assert_eq!(accepted(response).await?, entry);
		assert_eq!(listed(&admin).await?, json!([entry]));

		let response = sign_in(base, "other").await?;
		let other = signed_in(services, &response).await?;

		assert!(services.users.is_external(&other).await, "the invite makes it external");
		assert_eq!(listed(&admin).await?, json!([]), "the invite is used up");

		let response = call(&admin, Method::PUT, Some("someone@example.com"), &json!({})).await?;

		accepted(response).await?;

		let response =
			call(&admin, Method::DELETE, Some("Someone@Example.com"), &json!({})).await?;

		assert_eq!(accepted(response).await?, json!({}));
		assert_eq!(listed(&admin).await?, json!([]));

		let response =
			call(&admin, Method::DELETE, Some("someone@example.com"), &json!({})).await?;

		refusal(response, StatusCode::NOT_FOUND, "M_NOT_FOUND").await?;

		let response = call(&admin, Method::DELETE, Some("nodomain"), &json!({})).await?;

		refusal(response, StatusCode::BAD_REQUEST, "M_INVALID_PARAM").await?;

		Ok(())
	})
}

/// An address sent escaped is stored as written, an empty body gives no rooms
/// and a normal account, and writing the address again, in another case,
/// replaces its invite.
#[test]
fn put_defaults_escapes_and_replaces() -> Result {
	run_with("sso-registration-invites-put", NO_FIRST_ADMIN, async |services, base| {
		make_rooms(services, base).await?;

		let admin = administrator(services, base).await?;
		let entry = |external: bool| json!({ "email": "amy+chat@example.com", "rooms": [], "external": external });

		let response =
			call(&admin, Method::PUT, Some("amy%2Bchat%40example.com"), &json!({})).await?;

		assert_eq!(accepted(response).await?, entry(false));

		let again = json!({ "external": true });
		let response =
			call(&admin, Method::PUT, Some("AMY%2BCHAT%40example.com"), &again).await?;

		assert_eq!(accepted(response).await?, entry(true));
		assert_eq!(listed(&admin).await?, json!([entry(true)]));

		Ok(())
	})
}

/// A malformed address, something that is no room, an alias of another server,
/// and an address an account already has are refused, and store nothing.
#[test]
fn put_refuses_bad_input() -> Result {
	run_with("sso-registration-invites-bad-input", NO_FIRST_ADMIN, async |services, base| {
		make_rooms(services, base).await?;

		let admin = administrator(services, base).await?;

		let response = call(&admin, Method::PUT, Some("nodomain"), &json!({})).await?;

		refusal(response, StatusCode::BAD_REQUEST, "M_INVALID_PARAM").await?;

		let rooms = json!({ "rooms": ["nonsense"] });
		let response = call(&admin, Method::PUT, Some("other@school.example"), &rooms).await?;

		refusal(response, StatusCode::BAD_REQUEST, "M_BAD_JSON").await?;

		let rooms = json!({ "rooms": ["#welcome:elsewhere.example"] });
		let response = call(&admin, Method::PUT, Some("other@school.example"), &rooms).await?;
		let message = refusal(response, StatusCode::BAD_REQUEST, "M_INVALID_PARAM").await?;

		assert!(message.contains("#welcome:elsewhere.example"), "{message}");

		let response = sign_in(base, "guest").await?;
		let guest = signed_in(services, &response).await?;
		let response =
			call(&admin, Method::PUT, Some("guest@school.example"), &json!({})).await?;
		let message = refusal(response, StatusCode::BAD_REQUEST, "M_THREEPID_IN_USE").await?;

		assert!(message.contains(guest.as_str()), "{message}");
		assert_eq!(listed(&admin).await?, json!([]));

		Ok(())
	})
}

/// An account that is no server administrator reaches none of the routes: it
/// stores nothing and removes nothing.
#[test]
fn only_administrators_reach_the_routes() -> Result {
	run_with("sso-registration-invites-forbidden", NO_FIRST_ADMIN, async |services, base| {
		make_rooms(services, base).await?;
		invite(services, "kept@example.com", &[], false)?;

		let creator = creator(services, base);
		let requests = [
			(Method::GET, None),
			(Method::PUT, Some("other@school.example")),
			(Method::DELETE, Some("kept@example.com")),
		];

		for (method, email) in requests {
			let response = call(&creator, method, email, &json!({})).await?;

			refusal(response, StatusCode::FORBIDDEN, "M_FORBIDDEN").await?;
		}

		assert!(!pending(services, "other@school.example").await?, "nothing is stored");
		assert!(pending(services, "kept@example.com").await?, "nothing is removed");

		Ok(())
	})
}

/// `/versions` lists the feature on a server with an identity provider.
#[test]
fn versions_lists_the_feature() -> Result {
	run("sso-registration-invites-versions", async |services, base| {
		assert_eq!(advertised(services, base).await?, true);

		Ok(())
	})
}

/// Without an identity provider no sign-in reads an invite, and `/versions`
/// does not list the feature.
#[test]
fn versions_omits_the_feature_without_a_provider() -> Result {
	boot(
		"sso-registration-invites-no-provider",
		["log_enable=false"],
		async |services, base| {
			assert_eq!(advertised(services, base).await?, Value::Null);

			Ok(())
		},
	)
}
