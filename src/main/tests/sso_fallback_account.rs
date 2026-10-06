#![cfg(test)]

#[expect(
	dead_code,
	reason = "Only listener readiness is shared with the client API harness."
)]
mod client;

use std::{collections::HashMap, net::TcpListener, pin::pin, time::Duration};

use axum::{
	Form, Json, Router,
	extract::State,
	http::HeaderMap,
	routing::{get, post},
};
use axum_server::from_tcp;
use futures::future::{join, select};
use reqwest::{
	Client, Response, StatusCode, Url,
	header::{AUTHORIZATION, COOKIE, LOCATION, SET_COOKIE},
	redirect::Policy,
};
use serde_json::{Value, json};
use tuwunel::{Args, Runtime, Server, async_run, async_start, async_stop};
use tuwunel_core::{
	Err, Result, err,
	ruma::{OwnedUserId, UserId},
	utils::string::truncate_deterministic,
};
use tuwunel_service::{Services, oauth::unique_id_sub};

use self::client::wait_until_ready;

const IDP: &str = "test-idp";
const CLIENT: &str = "https://client.example/callback";

/// An identity whose claimed username is taken falls back to a username
/// derived from its subject, but is not handed an account another identity
/// already signs in to there.
#[test]
fn sso_fallback_account() -> Result {
	let listener = TcpListener::bind(("127.0.0.1", 0))?;
	let provider = TcpListener::bind(("127.0.0.1", 0))?;
	let port = listener.local_addr()?.port();
	let base = format!("http://127.0.0.1:{port}");
	let issuer = format!("http://{}", provider.local_addr()?);

	let option = |key: &str, value: &str| format!("identity_provider.{IDP}.{key}={value}");
	let args = Args::default_test(&["fresh", "cleanup"])
		.with_option("address=[\"127.0.0.1\"]")
		.with_option(format!("port={port}"))
		.with_option("listening=true")
		.with_option(format!("well_known.client=\"{base}\""))
		.with_option("sso_trusted_redirect_hosts=[\"client.example\"]")
		.with_option(option("client_id", &format!("\"{IDP}\"")))
		.with_option(option("client_secret", "\"test-secret\""))
		.with_option(option("brand", "\"test\""))
		.with_option(option("issuer_url", &format!("\"{issuer}\"")));

	provider.set_nonblocking(true)?;

	let runtime = Runtime::new(Some(&args))?;
	let server = Server::new(Some(&args), Some(&runtime))?;

	runtime.block_on(async {
		let services = async_start(&server).await?;

		drop(listener);

		let exercise = async {
			let serve = serve_provider(provider, issuer);
			let outcome = select(pin!(serve), pin!(exercise(&services, &base)))
				.await
				.factor_first()
				.0;

			let shutdown = server.server.shutdown();

			outcome.and(shutdown)
		};

		let (run, outcome) = join(async_run(&server), exercise).await;

		drop(services);
		async_stop(&server).await?;
		run?;

		outcome
	})
}

async fn exercise(services: &Services, base: &str) -> Result {
	wait_until_ready(services, base).await?;

	let client = Client::builder()
		.redirect(Policy::none())
		.timeout(Duration::from_secs(10))
		.build()?;

	let provider = services.oauth.providers.get(IDP).await?;
	let bob = unique_id_sub((&provider, "bob"))?;
	let fallback = truncate_deterministic(&bob, Some(15..23)).to_lowercase();

	let response = sign_in(&client, base, "alice", &fallback).await?;

	assert_eq!(signed_in(services, &response).await?, user(services, &fallback)?);

	let response = sign_in(&client, base, "bob", &fallback).await?;

	assert_eq!(response.status(), StatusCode::BAD_REQUEST);
	services
		.oauth
		.sessions
		.get_by_unique_id(&bob)
		.await
		.expect_err("bob's identity is linked to no account");

	Ok(())
}

/// Sign in at the provider as `sub` claiming `username`.
async fn sign_in(client: &Client, base: &str, sub: &str, username: &str) -> Result<Response> {
	let url = format!("{base}/_matrix/client/v3/login/sso/redirect/{IDP}");
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

	let url = format!("{base}/_matrix/client/unstable/login/sso/callback/{IDP}");
	let response = client
		.get(url)
		.query(&[("code", format!("{sub}:{username}").as_str()), ("state", &state)])
		.header(COOKIE, cookie)
		.send()
		.await?;

	Ok(response)
}

/// The account a sign-in finishing at the client was handed.
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

fn user(services: &Services, localpart: &str) -> Result<OwnedUserId> {
	UserId::parse_with_server_name(localpart, services.globals.server_name()).map_err(Into::into)
}

fn location(response: &Response) -> Result<Url> {
	let location = response
		.headers()
		.get(LOCATION)
		.ok_or_else(|| err!("{} response has no location", response.status()))?;

	Url::parse(location.to_str().expect("location header text")).map_err(Into::into)
}

fn parameter(url: &Url, name: &str) -> String {
	url.query_pairs()
		.find(|(key, _)| key == name)
		.expect("query parameter")
		.1
		.into_owned()
}

/// Identity provider stand-in.
///
/// The code a callback presents comes back as the access token, and the access
/// token as the subject and its claimed username, so each callback names the
/// identity it signs in as.
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

async fn token(Form(form): Form<HashMap<String, String>>) -> Json<Value> {
	Json(json!({
		"access_token": form["code"],
		"token_type": "Bearer",
	}))
}

async fn userinfo(headers: HeaderMap) -> Json<Value> {
	let (sub, username) = headers[AUTHORIZATION]
		.to_str()
		.expect("bearer token")
		.trim_start_matches("Bearer ")
		.split_once(':')
		.expect("subject and username");

	Json(json!({
		"sub": sub,
		"preferred_username": username,
	}))
}
