#![cfg(test)]

#[expect(
	dead_code,
	reason = "Only listener readiness and posting are shared with the client API harness."
)]
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
	Err, Result,
	config::check::reload,
	err,
	result::NotFound,
	ruma::{
		OwnedUserId, UserId,
		api::appservice::{Namespace, Namespaces, Registration, RegistrationInit},
	},
};
use tuwunel_database::Cbor;
use tuwunel_service::{
	Services,
	oauth::{Session, UserInfo, fallback_localpart, unique_id_sub},
	users::{DeactivationReason, PASSWORD_SENTINEL, Register},
};

use self::fixture::boot;

type TokenForm = HashMap<String, String>;

const IDP: &str = "test-idp";

const CLIENT: &str = "https://client.example/callback";

/// Token of the appservice that creates accounts ahead of their owners.
const AS_TOKEN: &str = "sso-assigned-migration-token";

/// Accounts the provider assigns, by email address. Alice's address is listed
/// in a different case than the provider sends it, and two addresses name
/// Dave's account.
const ASSIGNED: &str = r#"{
	"alice@EXAMPLE.com" = "@alice:localhost",
	"carol@example.com" = "@carol:localhost",
	"dave@example.com" = "@dave:localhost",
	"dave.second@example.com" = "@dave:localhost",
	"gina@example.com" = "@gina:localhost",
	"kim@example.com" = "@kim:localhost",
	"lee@example.com" = "@lee:localhost",
	"nobody@example.com" = "@nobody:localhost",
	"pat@example.com" = "@pat:localhost",
	"server@example.com" = "@conduit:localhost",
	"uma@example.com" = "@uma:localhost"
}"#;

/// Two default providers, so a sign-in at the first continues at the second,
/// carrying its account; the second assigns Hank's account.
const CHAIN: [&str; 2] = ["chain-a", "chain-b"];

/// A listed address signs in to its assigned account, and only to it.
///
/// Alice's account was registered by an appservice, in its exclusive
/// namespace, without a password: her first sign-in lands in it, its login
/// token logs in, and her next sign-in follows the stored link. Accounts with a
/// password or another system's marker keep it. Unlisted addresses, and a
/// listed one spelled with a non-ASCII lookalike, are mapped as before. A
/// listed address whose account is deactivated, linked to another identity
/// (also by adoption), missing, or the server user, or whose address the
/// provider says is unverified, or which names another account than a chained
/// sign-in carries, is refused, linked to nothing and given no other account,
/// and the account is left as it was. An address listed by a reload is
/// assigned at the next sign-in.
#[test]
fn sso_assigned_account() -> Result {
	let provider = TcpListener::bind(("127.0.0.1", 0))?;
	let issuer = format!("http://{}", provider.local_addr()?);

	let option =
		|idp: &str, key: &str, value: &str| format!("identity_provider.{idp}.{key}={value}");

	let assigned = ASSIGNED.replace(['\n', '\t'], "");
	let options = [IDP, CHAIN[0], CHAIN[1]]
		.into_iter()
		.flat_map(|idp| {
			[
				option(idp, "client_id", &format!("\"{idp}\"")),
				option(idp, "client_secret", "\"test-secret\""),
				option(idp, "brand", "\"test\""),
				option(idp, "issuer_url", &format!("\"{issuer}\"")),
				option(idp, "userid_claims", "[\"email\"]"),
			]
		})
		.chain([
			"oidc_registration_allowed_redirect_hosts=[\"client.example\"]".to_owned(),
			option(IDP, "assigned_accounts", &assigned),
			option(CHAIN[0], "default", "true"),
			option(CHAIN[1], "default", "true"),
			option(
				CHAIN[1],
				"assigned_accounts",
				r#"{ "hank@example.com" = "@hank:localhost" }"#,
			),
		])
		.collect::<Vec<_>>();

	provider.set_nonblocking(true)?;

	boot("sso-assigned-account", options, async move |services, base| {
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
/// the access token and userinfo answers as `sub` and an email address at
/// `example.com`, so each callback names the identity it signs in as.
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

	// Header values are ASCII, so the KELVIN SIGN, which lowercases to `k`, is
	// spelled out.
	let localpart = localpart.replace("KELVIN", "\u{212A}");
	let mut claims = json!({
		"sub": sub,
		"email": format!("{localpart}@example.com"),
	});

	// Some providers send the claim as a string.
	if sub.starts_with("unverified") {
		claims["email_verified"] = json!("false");
	}

	Json(claims)
}

async fn exercise(services: &Services, base: &str) -> Result {
	let client = Client::builder()
		.redirect(Policy::none())
		.timeout(Duration::from_secs(10))
		.build()?;

	register_appservice(services).await?;

	// A listed address lands in its account, in whatever case the provider
	// sends it, and the account becomes one SSO signs in to.
	let alice = register_ahead(services, base, "alice").await?;
	let response = sign_in(&client, base, "alice", "Alice").await?;
	let (user_id, _token) = log_in(&client, base, &response).await?;

	assert_eq!(user_id, alice);
	assert_eq!(services.users.origin(&alice).await?, "sso");
	assert_eq!(services.users.password_hash(&alice).await?, PASSWORD_SENTINEL);
	assert_eq!(linked_account(services, "alice").await?, alice);

	// The next sign-in follows the link.
	let response = sign_in(&client, base, "alice", "Alice").await?;

	assert_eq!(signed_in(services, &response).await?, alice);

	// An account with a password, or another sign-in system's marker, keeps it.
	let pat = UserId::parse_with_server_name("pat", services.globals.server_name())?;

	services
		.users
		.full_register(Register {
			user_id: Some(&pat),
			password: Some("sso-assigned-test-password"),
			..Default::default()
		})
		.await?;

	let hash = services.users.password_hash(&pat).await?;
	let response = sign_in(&client, base, "pat", "pat").await?;

	assert_eq!(signed_in(services, &response).await?, pat);
	assert_eq!(services.users.password_hash(&pat).await?, hash);
	assert_eq!(services.users.origin(&pat).await?, "password");
	assert_eq!(linked_account(services, "pat").await?, pat);

	let lee = UserId::parse_with_server_name("lee", services.globals.server_name())?;

	services
		.users
		.create(&lee, Some(PASSWORD_SENTINEL), Some("ldap"))
		.await?;

	let response = sign_in(&client, base, "lee", "lee").await?;

	assert_eq!(signed_in(services, &response).await?, lee);
	assert_eq!(services.users.origin(&lee).await?, "ldap");
	assert_eq!(linked_account(services, "lee").await?, lee);

	// A non-ASCII lookalike of a listed address is not that address.
	let kim = register_ahead(services, base, "kim").await?;
	let response = sign_in(&client, base, "kelvin", "KELVINim").await?;

	assert_eq!(signed_in(services, &response).await?, fallback(services, "kelvin").await?);
	assert_eq!(services.users.password_hash(&kim).await?, "");
	assert_eq!(services.users.origin(&kim).await?, "password");

	// Unlisted addresses are mapped as before: a free name gets a new account,
	// a taken one the identity's fallback.
	let response = sign_in(&client, base, "erin", "erin").await?;
	let erin = UserId::parse_with_server_name("erin", services.globals.server_name())?;

	assert_eq!(signed_in(services, &response).await?, erin);

	let bob = UserId::parse_with_server_name("bob", services.globals.server_name())?;

	services
		.users
		.full_register(Register {
			user_id: Some(&bob),
			password: Some("sso-assigned-test-password"),
			..Default::default()
		})
		.await?;

	let response = sign_in(&client, base, "bob", "bob").await?;

	assert_eq!(signed_in(services, &response).await?, fallback(services, "bob").await?);

	refusals(services, base, &client).await?;
	chained(services, base, &client).await?;
	reloaded(services, base, &client).await
}

/// Listed addresses whose sign-in is refused, each for one reason.
async fn refusals(services: &Services, base: &str, client: &Client) -> Result {
	// A deactivated account stays deactivated.
	let carol = register_ahead(services, base, "carol").await?;

	services
		.users
		.deactivate_account(&carol, DeactivationReason::SelfService)
		.await?;

	refused(services, client, base, "carol", "carol", "is deactivated").await?;
	assert_eq!(services.users.password_hash(&carol).await?, "");

	// An account linked to one identity is refused to another listed address.
	let dave = register_ahead(services, base, "dave").await?;
	let response = sign_in(client, base, "dave", "dave").await?;

	assert_eq!(signed_in(services, &response).await?, dave);

	refused(services, client, base, "dave-second", "dave.second", "is linked to another").await?;
	assert_eq!(linked_account(services, "dave").await?, dave);

	// A link adopted from another server's database has no entry in the user's
	// index, and still makes the account another identity's.
	let gina = register_ahead(services, base, "gina").await?;

	adopt(services, &gina, "gina-adopted").await?;
	refused(services, client, base, "gina", "gina", "is linked to another").await?;
	assert_eq!(linked_account(services, "gina-adopted").await?, gina);
	assert_eq!(services.users.password_hash(&gina).await?, "");

	// A missing account is not created.
	refused(services, client, base, "nobody", "nobody", "does not exist").await?;

	let nobody = UserId::parse_with_server_name("nobody", services.globals.server_name())?;

	assert!(!services.users.exists(&nobody).await, "nobody has no account");

	// The server user is never given out.
	let server_user = &services.globals.server_user;

	refused(services, client, base, "server", "server", "is the server user").await?;
	assert_eq!(services.users.password_hash(server_user).await?, "");
	assert_eq!(services.users.origin(server_user).await?, "password");

	// A provider that says the address is unverified is not believed; one that
	// says nothing is.
	let uma = register_ahead(services, base, "uma").await?;

	refused(services, client, base, "unverified-uma", "uma", "is unverified").await?;
	assert_eq!(services.users.password_hash(&uma).await?, "");

	let response = sign_in(client, base, "uma", "uma").await?;

	assert_eq!(signed_in(services, &response).await?, uma);

	Ok(())
}

/// A chained sign-in whose listed address names another account than the one
/// it carries is refused, and the carried account is left alone.
async fn chained(services: &Services, base: &str, client: &Client) -> Result {
	let hank = register_ahead(services, base, "hank").await?;
	let response = sign_in_at(client, base, CHAIN[0], "chain-first", "chainfirst").await?;
	let carried = UserId::parse_with_server_name("chainfirst", services.globals.server_name())?;

	assert_eq!(linked_account(services, "chain-first").await?, carried);

	// The first provider hands over to the second instead of the client.
	let state = parameter(&location(&response)?, "state");
	let cookie = cookie(&response);
	let url = format!("{base}/_matrix/client/unstable/login/sso/callback/{}", CHAIN[1]);
	let response = client
		.get(url)
		.query(&[("code", "chain-second:hank"), ("state", &state)])
		.header(COOKIE, cookie)
		.send()
		.await?;

	assert_eq!(response.status(), StatusCode::FORBIDDEN);

	let error: Value = response.json().await?;
	let message = error["error"].as_str().unwrap_or_default();

	assert!(message.contains("which it carries"), "{message}");
	assert!(unlinked(services, "chain-second").await?, "chain-second is linked to nothing");
	assert_eq!(linked_account(services, "chain-first").await?, carried);
	assert_eq!(services.users.password_hash(&hank).await?, "");

	Ok(())
}

/// An address a reload adds is assigned at the next sign-in.
async fn reloaded(services: &Services, base: &str, client: &Client) -> Result {
	let rita = register_ahead(services, base, "rita").await?;
	let response = sign_in(client, base, "rita-early", "rita").await?;

	assert_eq!(signed_in(services, &response).await?, fallback(services, "rita-early").await?);

	let mut config = (*services.server.config.clone()).clone();

	config
		.identity_provider
		.values_mut()
		.filter(|provider| provider.id() == IDP)
		.for_each(|provider| {
			provider
				.assigned_accounts
				.insert("rita@example.com".to_owned(), rita.clone());
		});

	reload(&services.config, &config)?;
	services.server.config.update(config)?;

	let response = sign_in(client, base, "rita", "rita").await?;

	assert_eq!(signed_in(services, &response).await?, rita);

	Ok(())
}

/// Register an appservice that creates accounts for the test's people.
///
/// Its namespace is exclusive, as a migration's usually is: SSO never names a
/// new account in it, so only the assignment signs the people in to theirs.
async fn register_appservice(services: &Services) -> Result {
	let namespaces = Namespaces {
		users: vec![Namespace::new(
			true,
			"^@(alice|carol|dave|gina|hank|kim|rita|uma):.*$".to_owned(),
		)],
		..Default::default()
	};

	let registration: Registration = RegistrationInit {
		id: "sso-assigned-migration".to_owned(),
		url: None,
		as_token: AS_TOKEN.to_owned(),
		hs_token: "sso-assigned-migration-hs-token".to_owned(),
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

/// Create an account through the appservice, without a password or a device.
async fn register_ahead(services: &Services, base: &str, localpart: &str) -> Result<OwnedUserId> {
	let appservice = client::Client { services, base, token: AS_TOKEN };
	let response = appservice
		.post(
			"register",
			&json!({
				"type": "m.login.application_service",
				"username": localpart,
				"inhibit_login": true,
			}),
		)
		.await?;

	Ok(client::field(&response, "user_id")?.try_into()?)
}

/// Sign in as `sub` with the address `<localpart>@example.com`, and expect a
/// refusal naming `reason` that links the identity to nothing and creates no
/// account for it.
async fn refused(
	services: &Services,
	client: &Client,
	base: &str,
	sub: &str,
	localpart: &str,
	reason: &str,
) -> Result {
	let response = sign_in(client, base, sub, localpart).await?;

	assert_eq!(response.status(), StatusCode::FORBIDDEN, "{localpart} is refused");

	let error: Value = response.json().await?;
	let message = error["error"].as_str().unwrap_or_default();

	assert_eq!(error["errcode"], "M_FORBIDDEN");
	assert!(message.contains(reason), "{localpart}: {message}");

	assert!(unlinked(services, sub).await?, "{sub} is linked to no account");
	assert!(
		!services
			.users
			.exists(&fallback(services, sub).await?)
			.await,
		"{sub} has no account"
	);

	Ok(())
}

/// Whether the identity `sub` is linked to no account. All the test's providers
/// share one issuer, so an identity is the same at each.
async fn unlinked(services: &Services, sub: &str) -> Result<bool> {
	let provider = services.oauth.providers.get(IDP).await?;
	let linked = services
		.oauth
		.sessions
		.get_by_unique_id(&unique_id_sub((&provider, sub))?)
		.await;

	Ok(linked.is_not_found())
}

/// Link the account to the identity `sub` as adopting another server's database
/// does: the session and its identity index, without the user's index.
async fn adopt(services: &Services, user_id: &UserId, sub: &str) -> Result {
	let provider = services.oauth.providers.get(IDP).await?;
	let unique_id = unique_id_sub((&provider, sub))?;
	let sess_id = format!("{sub}-session");
	let session = Session {
		idp_id: Some(IDP.to_owned()),
		sess_id: Some(sess_id.clone()),
		user_id: Some(user_id.to_owned()),
		user_info: Some(UserInfo {
			sub: sub.to_owned(),
			..Default::default()
		}),
		..Default::default()
	};

	services.db["oauthid_session"].raw_put(&sess_id, Cbor(&session));
	services.db["oauthuniqid_oauthid"].insert(unique_id.as_str(), &sess_id);

	Ok(())
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

/// The fallback account of the identity `sub`.
async fn fallback(services: &Services, sub: &str) -> Result<OwnedUserId> {
	let provider = services.oauth.providers.get(IDP).await?;
	let localpart = fallback_localpart(&unique_id_sub((&provider, sub))?);

	Ok(UserId::parse_with_server_name(
		localpart.as_str(),
		services.globals.server_name(),
	)?)
}

/// The account the identity `sub` is linked to.
async fn linked_account(services: &Services, sub: &str) -> Result<OwnedUserId> {
	let provider = services.oauth.providers.get(IDP).await?;
	let session = services
		.oauth
		.sessions
		.get_by_unique_id(&unique_id_sub((&provider, sub))?)
		.await?;

	session
		.user_id
		.ok_or_else(|| err!("{sub} is linked to no account"))
}

/// Sign in as `sub` with the address `<localpart>@example.com`.
///
/// The callback's response returns unfollowed, so the caller reads its status
/// and where it redirects.
async fn sign_in(client: &Client, base: &str, sub: &str, localpart: &str) -> Result<Response> {
	sign_in_at(client, base, IDP, sub, localpart).await
}

/// Sign in at the provider `idp` as `sub` with `<localpart>@example.com`.
async fn sign_in_at(
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
	let cookie = cookie(&redirect);
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
