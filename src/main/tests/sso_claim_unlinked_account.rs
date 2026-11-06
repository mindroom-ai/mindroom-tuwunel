#![cfg(test)]

mod client;
mod fixture;

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
	Client, Response, StatusCode, Url,
	header::{AUTHORIZATION, COOKIE, LOCATION, SET_COOKIE},
	redirect::Policy,
};
use serde_json::{Value, json};
use tuwunel_core::{
	Err, Result, err,
	ruma::{
		OwnedUserId, UserId,
		api::appservice::{Namespace, Namespaces, Registration, RegistrationInit},
	},
};
use tuwunel_service::{
	Services,
	oauth::{Session, UserInfo, fallback_localpart, unique_id_sub},
	tasks::Status,
	users::{DeactivationReason, PASSWORD_SENTINEL},
};

use self::fixture::boot;

type TokenForm = HashMap<String, String>;

/// Provider that lets a sign-in claim an account nobody can sign in to.
const CLAIM_IDP: &str = "claim-idp";

/// Provider with the option left off.
const PLAIN_IDP: &str = "plain-idp";

const CLIENT: &str = "https://client.example/callback";

/// Token of the appservice that creates accounts ahead of their owners.
const AS_TOKEN: &str = "sso-claim-migration-token";

/// The room every account joins when it signs up.
const LOBBY: &str = "#lobby:localhost";

/// Token of the local user who creates the lobby.
const HOST_TOKEN: &str = "sso-claim-host-access-token-0123456789";

/// Token that lets a person register an account for themselves.
const REGISTRATION_TOKEN: &str = "sso-claim-registration-token";

/// How long the background provisioning of a claim may take.
const PROVISIONING: Duration = Duration::from_secs(10);

/// A sign-in claims an account an appservice created without a password.
///
/// The plain provider leaves the account alone as before. At the claiming
/// provider the identity whose email names it gets it, as an SSO account linked
/// to that identity: its login token logs in, and one background task joins the
/// account to the auto-join room and gives it the identity's name. The next
/// sign-in reaches the same account and provisions nothing. A name someone gave
/// an account survives its claim. Identities naming an account that breaks one
/// rule each get their own fallback account and leave that account as it was:
/// one registered by its owner, one with a password, a deactivated one, a
/// locked one, one with a device, one linked to another identity, one an admin
/// association awaits, one another system signs in to, and the server user.
#[test]
fn sso_claim_unlinked_account() -> Result {
	let provider = TcpListener::bind(("127.0.0.1", 0))?;
	let issuer = format!("http://{}", provider.local_addr()?);

	let option =
		|idp: &str, key: &str, value: &str| format!("identity_provider.{idp}.{key}={value}");

	let options = [CLAIM_IDP, PLAIN_IDP]
		.into_iter()
		.flat_map(|idp| {
			[
				option(idp, "client_id", &format!("\"{idp}\"")),
				option(idp, "client_secret", "\"test-secret\""),
				option(idp, "brand", &format!("\"{idp}\"")),
				option(idp, "issuer_url", &format!("\"{issuer}\"")),
				option(idp, "userid_claims", "[\"email\"]"),
			]
		})
		.chain([
			option(CLAIM_IDP, "claim_unlinked_accounts", "true"),
			"oidc_registration_allowed_redirect_hosts=[\"client.example\"]".to_owned(),
			format!("auto_join_rooms=[\"{LOBBY}\"]"),
			"allow_registration=true".to_owned(),
			format!("registration_token=\"{REGISTRATION_TOKEN}\""),
		])
		.collect::<Vec<_>>();

	provider.set_nonblocking(true)?;

	boot("sso-claim-unlinked-account", options, async move |services, base| {
		let serve = serve_provider(provider, issuer);

		select(pin!(serve), pin!(exercise(services, base)))
			.await
			.factor_first()
			.0
	})
}

/// Serve the identity provider endpoints until the test is done.
///
/// A callback's code is `sub:localpart`, which the token endpoint returns as
/// the access token and userinfo answers as `sub`, an email address at
/// `example.com` and a name, so each callback names the identity it signs in
/// as.
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
	let (sub, localpart) = headers[AUTHORIZATION]
		.to_str()
		.expect("bearer token")
		.trim_start_matches("Bearer ")
		.split_once(':')
		.expect("subject and localpart");

	Json(json!({
		"sub": sub,
		"email": format!("{localpart}@example.com"),
		"name": format!("Name of {localpart}"),
	}))
}

async fn exercise(services: &Services, base: &str) -> Result {
	let client = Client::builder()
		.redirect(Policy::none())
		.timeout(Duration::from_secs(10))
		.build()?;

	register_appservice(services).await?;

	let host_id = client::register(services, "host", HOST_TOKEN).await?;
	let host = client::Client { services, base, token: HOST_TOKEN };
	let lobby = host
		.create_room(&json!({"room_alias_name": "lobby", "preset": "public_chat"}))
		.await?;

	let alice = register_ahead(services, base, "alice", true).await?;

	assert!(services.users.is_unclaimed(&alice).await, "alice is marked unclaimed");

	// With the option off, an identity naming the account gets its fallback.
	let response = sign_in(&client, base, PLAIN_IDP, "plain-alice", "alice").await?;

	assert_eq!(
		signed_in(services, &response).await?,
		fallback(services, PLAIN_IDP, "plain-alice").await?
	);
	assert_unclaimed(services, &alice, "password", "").await?;

	// With it on, the identity naming the account claims it and logs in.
	let response = sign_in(&client, base, CLAIM_IDP, "alice", "alice").await?;
	let (user_id, token) = log_in(&client, base, &response).await?;

	assert_eq!(user_id, alice);
	assert_eq!(services.users.origin(&alice).await?, "sso");
	assert_eq!(services.users.password_hash(&alice).await?, PASSWORD_SENTINEL);
	assert!(!services.users.is_unclaimed(&alice).await, "a claim removes the mark");
	assert_eq!(linked_account(services, "alice").await?, alice);

	// Provisioned once, as a new SSO account: the lobby and the identity's name.
	provisioned(services, &alice).await?;

	let suffix = &services.config.new_user_displayname_suffix;

	assert!(
		services
			.state_cache
			.is_joined(&alice, &lobby)
			.await,
		"alice joined the lobby"
	);
	assert_eq!(
		services.profile.displayname(&alice).await?,
		format!("Name of alice {suffix}").trim_end()
	);

	// The next sign-in follows the link and provisions nothing again.
	let alice_client = client::Client { services, base, token: &token };

	alice_client
		.post(&format!("rooms/{lobby}/leave"), &json!({}))
		.await?;

	let response = sign_in(&client, base, CLAIM_IDP, "alice", "alice").await?;

	assert_eq!(signed_in(services, &response).await?, alice);
	provisioned(services, &alice).await?;
	assert!(
		!services
			.state_cache
			.is_joined(&alice, &lobby)
			.await,
		"alice stays out"
	);

	// Another identity naming the claimed account gets its own.
	let response = sign_in(&client, base, CLAIM_IDP, "other-alice", "alice").await?;

	assert_eq!(
		signed_in(services, &response).await?,
		fallback(services, CLAIM_IDP, "other-alice").await?
	);
	assert_eq!(linked_account(services, "alice").await?, alice);

	// A name someone gave the account is kept.
	let gina = register_ahead(services, base, "gina", true).await?;

	services
		.profile
		.set_displayname(&gina, Some("Gina Example"), None)
		.await?;

	let response = sign_in(&client, base, CLAIM_IDP, "gina", "gina").await?;

	assert_eq!(signed_in(services, &response).await?, gina);
	provisioned(services, &gina).await?;
	assert_eq!(services.profile.displayname(&gina).await?, "Gina Example");
	assert!(
		services
			.state_cache
			.is_joined(&gina, &lobby)
			.await,
		"gina joined the lobby"
	);

	// An account its owner registered is theirs, though it has no password.
	let kim = register_self(&client, base, "kim").await?;

	refused(services, &client, base, "kim", "kim").await?;
	assert_unclaimed(services, &kim, "password", "").await?;

	// An account with a password is someone's.
	let bob = register_ahead(services, base, "bob", true).await?;

	services
		.users
		.set_password(&bob, Some("sso-claim-test-password"))
		.await?;

	let hash = services.users.password_hash(&bob).await?;

	refused(services, &client, base, "bob", "bob").await?;
	assert_unclaimed(services, &bob, "password", &hash).await?;

	// A deactivated account loses its mark, and stays deactivated even marked.
	let carol = register_ahead(services, base, "carol", true).await?;

	services
		.users
		.deactivate_account(&carol, DeactivationReason::SelfService)
		.await?;

	assert!(!services.users.is_unclaimed(&carol).await, "deactivation removes the mark");
	refused(services, &client, base, "carol", "carol").await?;

	services.users.mark_unclaimed(&carol);

	refused(services, &client, base, "carol-again", "carol").await?;
	assert_unclaimed(services, &carol, "password", "").await?;
	assert!(services.users.is_deactivated(&carol).await?, "carol stays deactivated");

	// A locked account waits for an admin.
	let lee = register_ahead(services, base, "lee", true).await?;

	services.users.set_locked(&lee, &host_id);

	refused(services, &client, base, "lee", "lee").await?;
	assert_unclaimed(services, &lee, "password", "").await?;

	// Whoever holds a device's token or keys already uses the account.
	let dave = register_ahead(services, base, "dave", false).await?;

	refused(services, &client, base, "dave", "dave").await?;
	assert_unclaimed(services, &dave, "password", "").await?;

	// An account linked to an identity without a claim, as adopting another
	// server's links does, belongs to that identity.
	let hank = register_ahead(services, base, "hank", true).await?;

	link(services, &hank, "hank-elsewhere").await;
	refused(services, &client, base, "hank", "hank").await?;
	assert_eq!(services.users.password_hash(&hank).await?, "");
	assert_eq!(linked_account(services, "hank-elsewhere").await?, hank);

	// An account an admin set aside for an identity waits for that identity.
	let erin = register_ahead(services, base, "erin", true).await?;
	let claims = [("sub".to_owned(), "erin-elsewhere".to_owned())].into();

	services
		.oauth
		.sessions
		.set_user_association_pending(CLAIM_IDP, &erin, claims);

	refused(services, &client, base, "erin", "erin").await?;
	assert_unclaimed(services, &erin, "password", "").await?;

	// Another system signs in to an account of its origin.
	let ivan = register_ahead(services, base, "ivan", true).await?;

	services.users.set_origin(&ivan, "jwt");
	services
		.users
		.set_password(&ivan, Some(PASSWORD_SENTINEL))
		.await?;

	refused(services, &client, base, "ivan", "ivan").await?;
	assert_unclaimed(services, &ivan, "jwt", PASSWORD_SENTINEL).await?;

	// The server user is never claimed, even marked.
	let server_user = &services.globals.server_user;

	services.users.mark_unclaimed(server_user);

	refused(services, &client, base, "server", server_user.localpart()).await?;
	assert_unclaimed(services, server_user, "password", "").await?;

	Ok(())
}

/// Register an appservice that may create accounts for the test's people.
///
/// Its namespace is not exclusive, so the people may sign in themselves, as
/// during a migration that fills their accounts before they do.
async fn register_appservice(services: &Services) -> Result {
	let namespaces = Namespaces {
		users: vec![Namespace::new(
			false,
			"^@(alice|bob|carol|dave|erin|gina|hank|ivan|lee):.*$".to_owned(),
		)],
		..Default::default()
	};

	let registration: Registration = RegistrationInit {
		id: "sso-claim-migration".to_owned(),
		url: None,
		as_token: AS_TOKEN.to_owned(),
		hs_token: "sso-claim-migration-hs-token".to_owned(),
		sender_localpart: "migration".to_owned(),
		namespaces,
		rate_limited: None,
		protocols: None,
	}
	.into();

	services
		.appservice
		.register_appservice(registration)
		.await
}

/// Create an account through the appservice, without a password, and without a
/// device when `inhibit_login` is set.
async fn register_ahead(
	services: &Services,
	base: &str,
	localpart: &str,
	inhibit_login: bool,
) -> Result<OwnedUserId> {
	let appservice = client::Client { services, base, token: AS_TOKEN };
	let response = appservice
		.post(
			"register",
			&json!({
				"type": "m.login.application_service",
				"username": localpart,
				"inhibit_login": inhibit_login,
			}),
		)
		.await?;

	Ok(client::field(&response, "user_id")?.try_into()?)
}

/// Log in with a finished sign-in's login token, as a client does, and return
/// the account and its access token.
async fn log_in(
	client: &Client,
	base: &str,
	response: &Response,
) -> Result<(OwnedUserId, String)> {
	let destination = location(response)?;

	if !destination.as_str().starts_with(CLIENT) {
		return Err!("sign-in finished at {destination} rather than the client");
	}

	let response: Value = client
		.post(format!("{base}/_matrix/client/v3/login"))
		.json(&json!({
			"type": "m.login.token",
			"token": parameter(&destination, "loginToken"),
		}))
		.send()
		.await?
		.error_for_status()?
		.json()
		.await?;

	let user_id = client::field(&response, "user_id")?.try_into()?;
	let token = client::field(&response, "access_token")?.to_owned();

	Ok((user_id, token))
}

/// Register an account for oneself, without a password and a device, with the
/// registration token.
async fn register_self(client: &Client, base: &str, localpart: &str) -> Result<OwnedUserId> {
	let url = format!("{base}/_matrix/client/v3/register");
	let body = json!({"username": localpart, "inhibit_login": true});
	let challenge: Value = client
		.post(url.as_str())
		.json(&body)
		.send()
		.await?
		.json()
		.await?;
	let session = challenge["session"]
		.as_str()
		.ok_or_else(|| err!("registration asked for no session"))?;

	let auth = json!({
		"type": "m.login.registration_token",
		"token": REGISTRATION_TOKEN,
		"session": session,
	});

	let mut body = body;
	body["auth"] = auth;

	let response: Value = client
		.post(url.as_str())
		.json(&body)
		.send()
		.await?
		.error_for_status()?
		.json()
		.await?;

	Ok(client::field(&response, "user_id")?.try_into()?)
}

/// Link the account to an identity of the claiming provider directly, without
/// a sign-in.
async fn link(services: &Services, user_id: &UserId, sub: &str) {
	let session = Session {
		idp_id: Some(CLAIM_IDP.to_owned()),
		sess_id: Some(format!("{sub}-session")),
		user_id: Some(user_id.to_owned()),
		user_info: Some(UserInfo {
			sub: sub.to_owned(),
			..Default::default()
		}),
		..Default::default()
	};

	services.oauth.sessions.put(&session).await;
}

/// Wait for the account's one provisioning task to complete.
async fn provisioned(services: &Services, user_id: &UserId) -> Result {
	let complete = client::poll_until(PROVISIONING, async || {
		services
			.tasks
			.by_resource(user_id.as_str())
			.iter()
			.any(|task| task.status == Status::Complete)
	})
	.await;

	let tasks = services.tasks.by_resource(user_id.as_str());

	assert!(complete, "{user_id} was provisioned: {tasks:?}");
	assert_eq!(tasks.len(), 1, "{user_id} was provisioned once: {tasks:?}");

	Ok(())
}

/// Sign in at the claiming provider as `<name>-sub` naming `localpart`, and
/// expect the identity's fallback account.
async fn refused(
	services: &Services,
	client: &Client,
	base: &str,
	name: &str,
	localpart: &str,
) -> Result {
	let sub = format!("{name}-sub");
	let response = sign_in(client, base, CLAIM_IDP, &sub, localpart).await?;

	assert_eq!(
		signed_in(services, &response).await?,
		fallback(services, CLAIM_IDP, &sub).await?,
		"{localpart} is not claimed"
	);

	Ok(())
}

/// The account keeps its origin and stored password and has no identity.
async fn assert_unclaimed(
	services: &Services,
	user_id: &UserId,
	origin: &str,
	password: &str,
) -> Result {
	assert_eq!(services.users.origin(user_id).await?, origin, "{user_id} origin");
	assert_eq!(services.users.password_hash(user_id).await?, password, "{user_id} password");
	assert!(!services.oauth.sessions.is_linked(user_id).await, "{user_id} is unlinked");

	Ok(())
}

/// The fallback account of the identity `sub` at a provider.
async fn fallback(services: &Services, idp: &str, sub: &str) -> Result<OwnedUserId> {
	let provider = services.oauth.providers.get(idp).await?;
	let localpart = fallback_localpart(&unique_id_sub((&provider, sub))?);

	Ok(UserId::parse_with_server_name(
		localpart.as_str(),
		services.globals.server_name(),
	)?)
}

/// The account the claiming provider's identity `sub` is linked to.
async fn linked_account(services: &Services, sub: &str) -> Result<OwnedUserId> {
	let provider = services.oauth.providers.get(CLAIM_IDP).await?;
	let session = services
		.oauth
		.sessions
		.get_by_unique_id(&unique_id_sub((&provider, sub))?)
		.await?;

	session
		.user_id
		.ok_or_else(|| err!("{sub} is linked to no account"))
}

/// Sign in at a provider as `sub` with an email address naming `localpart`.
///
/// The callback's response returns unfollowed, so the caller reads where it
/// redirects.
async fn sign_in(
	client: &Client,
	base: &str,
	idp: &str,
	sub: &str,
	localpart: &str,
) -> Result<Response> {
	let url = format!("{base}/_matrix/client/v3/login/sso/redirect/{idp}");
	let redirect = client
		.get(url)
		.query(&[("redirectUrl", CLIENT)])
		.send()
		.await?;

	assert_eq!(redirect.status(), StatusCode::FOUND);

	let state = parameter(&location(&redirect)?, "state");
	let cookie = redirect.headers()[SET_COOKIE]
		.to_str()
		.expect("cookie header text")
		.split(';')
		.next()
		.expect("cookie pair");

	let url = format!("{base}/_matrix/client/unstable/login/sso/callback/{idp}");
	let code = format!("{sub}:{localpart}");

	client
		.get(url)
		.query(&[("code", code.as_str()), ("state", &state)])
		.header(COOKIE, cookie)
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
