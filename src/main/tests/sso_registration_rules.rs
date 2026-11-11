#![cfg(test)]

mod client;
mod fixture;

#[path = "client/get.rs"]
mod get;

use std::{collections::HashMap, net::TcpListener, pin::pin, time::Duration};

use axum::{
	Form, Json, Router,
	extract::State,
	http::HeaderMap,
	routing::{get, post},
};
use axum_server::from_tcp;
use futures::future::select;
use reqwest::{
	Response, StatusCode, Url,
	header::{AUTHORIZATION, COOKIE, LOCATION, SET_COOKIE},
	redirect::Policy,
};
use serde_json::{Value, json};
use tuwunel_core::{
	Err, Result,
	config::{RegistrationRule, check::reload},
	err,
	result::NotFound,
	ruma::{
		OwnedRoomId, OwnedRoomOrAliasId, OwnedUserId, RoomId, UserId,
		events::room::member::MembershipState,
	},
};
use tuwunel_service::{
	Services,
	oauth::{fallback_localpart, unique_id_sub},
};

use self::{
	client::{Client, register},
	fixture::boot,
};

type TokenForm = HashMap<String, String>;

const IDP: &str = "test-idp";

const CLIENT: &str = "https://client.example/callback";

const CREATOR_TOKEN: &str = "sso-registration-rules-creator-token";

/// The provider's rules: a hosted domain for `#staff` and `#open`, one
/// address for `#class`, an address the provider says is unverified, and a
/// hosted domain whose first room this server never joined.
const RULES: &str = r##"[
	{ claims = { hd = "example.com" }, rooms = ["#staff:localhost", "#open:localhost"] },
	{ claims = { email = "guest@school.example" }, rooms = ["#class:localhost"] },
	{ claims = { email = "guest2@school.example" }, rooms = ["#class:localhost"] },
	{ claims = { hd = "far.example" }, rooms = ["!never:localhost", "#open:localhost"] }
]"##;

const REFUSAL: &str =
	"This sign-in may not create an account here; ask an administrator to invite you.";

const REASON: &str = "Automatically joining this room upon registration";

/// The rooms the rules name, made by one local account.
struct Rooms {
	staff: OwnedRoomId,
	open: OwnedRoomId,
	class: OwnedRoomId,
}

/// An identity meeting the hosted-domain rule gets an account joined to its
/// rooms and no other: the invite-only room after an invite from its creator,
/// the public one without, each with the registration reason.
#[test]
fn hosted_domain_joins_its_rooms() -> Result {
	run("sso-registration-rules-hd", async |services, base| {
		let rooms = make_rooms(services, base).await?;
		let response = sign_in(base, "amy").await?;
		let amy = signed_in(services, &response).await?;
		let creator = creator(services, base);

		assert_eq!(membership(services, &amy, &rooms.staff).await, Some(MembershipState::Join));
		assert_eq!(membership(services, &amy, &rooms.open).await, Some(MembershipState::Join));
		assert_eq!(membership(services, &amy, &rooms.class).await, None);

		let staff_invite = member_event(&creator, &rooms.staff, &amy, "invite").await?;
		let staff_join = member_event(&creator, &rooms.staff, &amy, "join").await?;
		let open_join = member_event(&creator, &rooms.open, &amy, "join").await?;

		assert_eq!(
			staff_invite.map(|event| event["sender"].clone()),
			Some(json!("@creator:localhost"))
		);
		assert_eq!(member_event(&creator, &rooms.open, &amy, "invite").await?, None);

		for join in [staff_join, open_join] {
			assert_eq!(join.map(|event| event["content"]["reason"].clone()), Some(json!(REASON)));
		}

		Ok(())
	})
}

/// A verified address in another case than its rule spells it gets an account
/// joined to that rule's room only.
#[test]
fn verified_address_joins_its_room() -> Result {
	run("sso-registration-rules-email", async |services, base| {
		let rooms = make_rooms(services, base).await?;
		let response = sign_in(base, "guest").await?;
		let guest = signed_in(services, &response).await?;

		assert_eq!(membership(services, &guest, &rooms.class).await, Some(MembershipState::Join));
		assert_eq!(membership(services, &guest, &rooms.staff).await, None);
		assert_eq!(membership(services, &guest, &rooms.open).await, None);

		Ok(())
	})
}

/// An identity meeting no rule is refused, and gets no account and no hold on
/// one; once a reload lists its address, the same identity signs in and joins.
#[test]
fn unlisted_address_is_refused_until_listed() -> Result {
	run("sso-registration-rules-unlisted", async |services, base| {
		let rooms = make_rooms(services, base).await?;

		refused(services, base, "other").await?;

		update_rules(services, |rules| {
			rules.push(RegistrationRule {
				claims: [("email".to_owned(), "other@school.example".to_owned())].into(),
				rooms: vec![alias("#class:localhost")],
			});
		})?;

		let response = sign_in(base, "other").await?;
		let other = signed_in(services, &response).await?;

		assert_eq!(other, user_id(services, "other")?);
		assert_eq!(membership(services, &other, &rooms.class).await, Some(MembershipState::Join));

		Ok(())
	})
}

/// A listed address the provider says is unverified meets no rule.
#[test]
fn unverified_address_is_refused() -> Result {
	run("sso-registration-rules-unverified", async |services, base| {
		make_rooms(services, base).await?;
		refused(services, base, "guest2").await
	})
}

/// An account made before the rules were set signs in although it meets none.
#[test]
fn existing_account_signs_in() -> Result {
	run("sso-registration-rules-existing", async |services, base| {
		let rules = current_rules(services)?;

		update_rules(services, Vec::clear)?;

		let response = sign_in(base, "early").await?;
		let early = signed_in(services, &response).await?;

		update_rules(services, |current| *current = rules)?;

		let response = sign_in(base, "early").await?;

		assert_eq!(signed_in(services, &response).await?, early);

		Ok(())
	})
}

/// A rule room this server never joined fails alone: the sign-in succeeds and
/// the rule's other room is joined.
#[test]
fn room_never_joined_is_skipped() -> Result {
	run("sso-registration-rules-never", async |services, base| {
		let rooms = make_rooms(services, base).await?;
		let response = sign_in(base, "far").await?;
		let far = signed_in(services, &response).await?;

		assert_eq!(membership(services, &far, &rooms.open).await, Some(MembershipState::Join));

		Ok(())
	})
}

/// An invite the server accepts on the account's behalf as well does no harm:
/// whichever join lands first stands.
#[test]
fn auto_accepted_invite_is_harmless() -> Result {
	let options = ["rooms.invites.auto_accept=true"];

	run_with("sso-registration-rules-auto-accept", &options, async |services, base| {
		let rooms = make_rooms(services, base).await?;
		let response = sign_in(base, "amy").await?;
		let amy = signed_in(services, &response).await?;

		assert_eq!(membership(services, &amy, &rooms.staff).await, Some(MembershipState::Join));
		assert_eq!(membership(services, &amy, &rooms.open).await, Some(MembershipState::Join));

		Ok(())
	})
}

/// Boot a server whose provider has the rules, with the provider stand-in
/// serving beside it, and run one exercise.
fn run<Exercise>(name: &str, exercise: Exercise) -> Result
where
	Exercise: AsyncFnOnce(&Services, &str) -> Result,
{
	run_with(name, &[], exercise)
}

/// `run`, with further server options.
fn run_with<Exercise>(name: &str, extra: &[&str], exercise: Exercise) -> Result
where
	Exercise: AsyncFnOnce(&Services, &str) -> Result,
{
	let provider = TcpListener::bind(("127.0.0.1", 0))?;
	let issuer = format!("http://{}", provider.local_addr()?);

	provider.set_nonblocking(true)?;

	let options = provider_options(&issuer)
		.into_iter()
		.chain(extra.iter().copied().map(str::to_owned))
		.collect::<Vec<_>>();

	boot(name, options.iter().map(String::as_str), async move |services, base| {
		let serve = serve_provider(provider, issuer);

		select(pin!(serve), pin!(exercise(services, base)))
			.await
			.factor_first()
			.0
	})
}

/// The provider's settings. Every boot leaves the process-wide tracing
/// subscriber alone, so the tests of this binary boot their servers side by
/// side.
fn provider_options(issuer: &str) -> Vec<String> {
	let option = |key: &str, value: &str| format!("identity_provider.{IDP}.{key}={value}");

	vec![
		option("client_id", &format!("\"{IDP}\"")),
		option("client_secret", "\"test-secret\""),
		option("brand", "\"test\""),
		option("issuer_url", &format!("\"{issuer}\"")),
		option("userid_claims", "[\"email\"]"),
		option("registration_rules", &RULES.replace(['\n', '\t'], "")),
		"oidc_registration_allowed_redirect_hosts=[\"client.example\"]".to_owned(),
		"log_enable=false".to_owned(),
	]
}

/// Make `#staff` and `#class`, both invite-only, and the public `#open`, as one
/// local account.
async fn make_rooms(services: &Services, base: &str) -> Result<Rooms> {
	register(services, "creator", CREATOR_TOKEN).await?;

	let creator = creator(services, base);
	let room = async |preset: &str, alias: &str| {
		creator
			.create_room(&json!({ "preset": preset, "room_alias_name": alias }))
			.await
	};

	Ok(Rooms {
		staff: room("private_chat", "staff").await?,
		open: room("public_chat", "open").await?,
		class: room("private_chat", "class").await?,
	})
}

fn creator<'a>(services: &'a Services, base: &'a str) -> Client<'a> {
	Client { services, base, token: CREATOR_TOKEN }
}

async fn membership(
	services: &Services,
	user_id: &UserId,
	room_id: &RoomId,
) -> Option<MembershipState> {
	services
		.state_cache
		.user_membership(user_id, room_id)
		.await
}

/// The user's membership event of the kind, as the room's creator reads it in
/// the room's history.
async fn member_event(
	creator: &Client<'_>,
	room_id: &RoomId,
	user_id: &UserId,
	membership: &str,
) -> Result<Option<Value>> {
	let messages = creator
		.get(&format!("rooms/{room_id}/messages?dir=b&limit=100"))
		.await?;

	Ok(messages["chunk"]
		.as_array()
		.into_iter()
		.flatten()
		.find(|event| {
			event["type"] == "m.room.member"
				&& event["state_key"] == user_id.as_str()
				&& event["content"]["membership"] == membership
		})
		.cloned())
}

/// Sign in as `sub` and expect the refusal, linking the identity to nothing
/// and leaving the account its address names absent.
async fn refused(services: &Services, base: &str, sub: &str) -> Result {
	let response = sign_in(base, sub).await?;

	assert_eq!(response.status(), StatusCode::FORBIDDEN, "{sub} is refused");

	let error: Value = response.json().await?;

	assert_eq!(error["errcode"], "M_FORBIDDEN");
	assert_eq!(error["error"], format!("M_FORBIDDEN: {REFUSAL}"));
	assert!(unlinked(services, sub).await?, "{sub} is linked to no account");

	let creator = creator(services, base);

	for user_id in [user_id(services, sub)?, fallback(services, sub).await?] {
		assert!(!services.users.exists(&user_id).await, "{user_id} has no account");

		let profile = creator
			.get_url(&creator.url(&format!("profile/{user_id}")))
			.await?;

		assert_eq!(profile.status(), StatusCode::NOT_FOUND, "{user_id} has no profile");
	}

	Ok(())
}

fn current_rules(services: &Services) -> Result<Vec<RegistrationRule>> {
	Ok(services
		.oauth
		.providers
		.find_config(IDP)?
		.registration_rules
		.clone())
}

/// Change the provider's rules through a config reload.
fn update_rules<Update>(services: &Services, update: Update) -> Result
where
	Update: FnOnce(&mut Vec<RegistrationRule>),
{
	let mut config = (*services.server.config.clone()).clone();
	let provider = config
		.identity_provider
		.values_mut()
		.find(|provider| provider.id() == IDP)
		.ok_or_else(|| err!("the provider is configured"))?;

	update(&mut provider.registration_rules);
	reload(&services.config, &config)?;
	services.server.config.update(config)?;

	Ok(())
}

fn alias(alias: &str) -> OwnedRoomOrAliasId { alias.try_into().expect("a room alias") }

fn user_id(services: &Services, localpart: &str) -> Result<OwnedUserId> {
	Ok(UserId::parse_with_server_name(localpart, services.globals.server_name())?)
}

/// Serve the identity provider endpoints until the test is done.
///
/// A callback's code is the identity's `sub`, which the token endpoint returns
/// as the access token and userinfo answers with that identity's claims.
async fn serve_provider(listener: TcpListener, issuer: String) -> Result {
	let app = Router::new()
		.route("/.well-known/openid-configuration", get(discover))
		.route("/token", post(token))
		.route("/userinfo", get(userinfo))
		.with_state(issuer);

	from_tcp(listener)?
		.serve(app.into_make_service())
		.await?;

	Err!("the identity provider stand-in stopped serving")
}

async fn discover(State(issuer): State<String>) -> Json<Value> {
	Json(json!({
		"issuer": issuer,
		"authorization_endpoint": format!("{issuer}/authorize"),
		"token_endpoint": format!("{issuer}/token"),
		"userinfo_endpoint": format!("{issuer}/userinfo"),
	}))
}

async fn token(Form(form): Form<TokenForm>) -> Json<Value> {
	Json(json!({
		"access_token": form["code"],
		"token_type": "Bearer",
	}))
}

async fn userinfo(headers: HeaderMap) -> Json<Value> {
	let sub = headers[AUTHORIZATION]
		.to_str()
		.expect("bearer token")
		.trim_start_matches("Bearer ");

	Json(claims(sub))
}

/// The claims the provider sends for each of the test's identities.
fn claims(sub: &str) -> Value {
	match sub {
		| "amy" => json!({ "sub": sub, "email": "amy@example.com", "hd": "example.com" }),
		| "guest" => json!({
			"sub": sub,
			"email": "GUEST@school.example",
			"email_verified": true,
		}),
		| "other" => json!({
			"sub": sub,
			"email": "other@school.example",
			"email_verified": true,
		}),
		| "guest2" => json!({
			"sub": sub,
			"email": "guest2@school.example",
			"email_verified": false,
		}),
		| "far" => json!({ "sub": sub, "email": "far@far.example", "hd": "far.example" }),
		| _ => json!({ "sub": sub, "email": format!("{sub}@elsewhere.example") }),
	}
}

/// Whether the identity `sub` is linked to no account.
async fn unlinked(services: &Services, sub: &str) -> Result<bool> {
	let provider = services.oauth.providers.get(IDP).await?;
	let linked = services
		.oauth
		.sessions
		.get_by_unique_id(&unique_id_sub((&provider, sub))?)
		.await;

	Ok(linked.is_not_found())
}

/// The fallback account of the identity `sub`.
async fn fallback(services: &Services, sub: &str) -> Result<OwnedUserId> {
	let provider = services.oauth.providers.get(IDP).await?;
	let localpart = fallback_localpart(&unique_id_sub((&provider, sub))?);

	user_id(services, &localpart)
}

/// Sign in as `sub`, returning the callback's unfollowed response.
async fn sign_in(base: &str, sub: &str) -> Result<Response> {
	let client = reqwest::Client::builder()
		.redirect(Policy::none())
		.timeout(Duration::from_secs(10))
		.build()?;

	let url = format!("{base}/_matrix/client/v3/login/sso/redirect/{IDP}");
	let redirect = client
		.get(url)
		.query(&[("redirectUrl", CLIENT)])
		.send()
		.await?;

	assert_eq!(redirect.status(), StatusCode::FOUND);

	let state = parameter(&location(&redirect)?, "state");
	let url = format!("{base}/_matrix/client/unstable/login/sso/callback/{IDP}");

	client
		.get(url)
		.query(&[("code", sub), ("state", &state)])
		.header(COOKIE, cookie(&redirect))
		.send()
		.await
		.map_err(Into::into)
}

/// Resolve the account named by a finished sign-in's login token.
///
/// Fails when the callback redirected anywhere but the client.
async fn signed_in(services: &Services, response: &Response) -> Result<OwnedUserId> {
	let destination = location(response)?;

	if !destination.as_str().starts_with(CLIENT) {
		return Err!("sign-in finished at {destination} rather than the client");
	}

	services
		.users
		.find_from_login_token(&parameter(&destination, "loginToken"))
		.await
}

/// The grant cookie a response sets, as a request sends it back.
fn cookie(response: &Response) -> &str {
	response.headers()[SET_COOKIE]
		.to_str()
		.expect("cookie header text")
		.split(';')
		.next()
		.expect("cookie pair")
}

fn location(response: &Response) -> Result<Url> {
	response
		.headers()
		.get(LOCATION)
		.ok_or_else(|| err!("{} response has no location", response.status()))?
		.to_str()
		.expect("location header text")
		.parse()
		.map_err(Into::into)
}

fn parameter(url: &Url, name: &str) -> String {
	url.query_pairs()
		.find(|(key, _)| key == name)
		.expect("query parameter")
		.1
		.into_owned()
}
