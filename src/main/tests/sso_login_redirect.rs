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
};
use tuwunel_service::{Services, users::Register};

use self::client::wait_until_ready;

const FIRST: &str = "first-idp";
const SECOND: &str = "second-idp";
const CLIENT: &str = "https://client.example/callback";
const TRUSTED: &str = "https://trusted.example/callback";

/// Both providers are `default`, so a sign-in at the first continues at the
/// second for the same account, and a sign-in at the second ends there.
#[test]
fn sso_login_redirect() -> Result {
	let listener = TcpListener::bind(("127.0.0.1", 0))?;
	let provider = TcpListener::bind(("127.0.0.1", 0))?;
	let port = listener.local_addr()?.port();
	let base = format!("http://127.0.0.1:{port}");
	let issuer = format!("http://{}", provider.local_addr()?);

	let args = Args::default_test(&["fresh", "cleanup"])
		.with_option("address=[\"127.0.0.1\"]")
		.with_option(format!("port={port}"))
		.with_option("listening=true")
		.with_option(format!("well_known.client=\"{base}\""))
		.with_option("oidc_registration_allowed_redirect_hosts=[\"trusted.example\"]")
		.with_option(
			"sso_trusted_redirect_hosts=[\"client.example\", \"web.example\", \"exampleapp\"]",
		);

	let args = [FIRST, SECOND]
		.into_iter()
		.fold(args, |args, id| {
			let option = |key: &str, value: &str| format!("identity_provider.{id}.{key}={value}");

			args.with_option(option("client_id", &format!("\"{id}\"")))
				.with_option(option("client_secret", "\"test-secret\""))
				.with_option(option("brand", "\"test\""))
				.with_option(option("issuer_url", &format!("\"{issuer}\"")))
				.with_option(option("default", "true"))
		});

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

	link_token_binds_nothing(services, &client, base).await?;
	chain_carries_the_account(services, &client, base).await?;
	unvetted_target_asks_first(services, &client, base).await?;

	Ok(())
}

/// A `loginToken` in a sign-in link is not the browser's to present: the
/// identity that signs in keeps its own account rather than joining the
/// token owner's.
async fn link_token_binds_nothing(services: &Services, client: &Client, base: &str) -> Result {
	let owner = user(services, "carol")?;
	let token = "sso-login-redirect-link-token";

	services
		.users
		.full_register(Register {
			user_id: Some(&owner),
			password: Some("sso-login-redirect-password"),
			..Default::default()
		})
		.await?;

	_ = services.users.create_login_token(&owner, token);

	let query = [("redirectUrl", CLIENT), ("loginToken", token)];
	let response = start(client, base, SECOND, &query).await?;
	let response = callback(client, base, SECOND, &response, "alice").await?;

	assert_eq!(signed_in(services, &response).await?, user(services, "alice")?);

	Ok(())
}

/// The first provider hands the browser straight to the second, and the
/// identity there joins the account the first one signed in.
async fn chain_carries_the_account(services: &Services, client: &Client, base: &str) -> Result {
	let response = start(client, base, FIRST, &[("redirectUrl", CLIENT)]).await?;
	let response = callback(client, base, FIRST, &response, "bob").await?;
	let next = location(&response)?;

	assert_eq!(response.status(), StatusCode::FOUND);
	assert_eq!(next.path(), "/authorize");
	assert_eq!(parameter(&next, "client_id"), SECOND);
	assert!(
		!next
			.query_pairs()
			.any(|(key, _)| key == "loginToken")
	);

	let response = callback(client, base, SECOND, &response, "bob-elsewhere").await?;

	assert_eq!(signed_in(services, &response).await?, user(services, "bob")?);

	Ok(())
}

/// A login token goes straight only to this server or a listed host. Any other
/// target is named on a page, and only following its link delivers the token.
async fn unvetted_target_asks_first(services: &Services, client: &Client, base: &str) -> Result {
	let own = format!("{base}/_tuwunel/oidc/_complete?oidc_req_id=x");
	let dave = user(services, "dave")?;

	for (target, asks) in [
		(TRUSTED, false),
		(own.as_str(), false),
		("https://web.example/login", false),
		("exampleapp://auth/login", false),
		("https://unlisted.example/", true),
		("element://connect", true),
	] {
		let response = start(client, base, SECOND, &[("redirectUrl", target)]).await?;
		let response = callback(client, base, SECOND, &response, "dave").await?;

		let destination = if asks {
			assert_eq!(response.status(), StatusCode::OK);
			assert!(!response.headers().contains_key(LOCATION));

			let html = response.text().await?;
			let href = html
				.split("href=\"")
				.find_map(|part| {
					part.split('"')
						.next()
						.filter(|href| href.contains("loginToken"))
				})
				.expect("continue link");

			assert!(html.contains(&format!("<strong>{target}</strong>")));

			Url::parse(&href.replace("&amp;", "&"))?
		} else {
			assert_eq!(response.status(), StatusCode::FOUND);

			location(&response)?
		};

		assert!(destination.as_str().starts_with(target));
		assert_eq!(token_owner(services, &destination).await?, dave);
	}

	for target in ["javascript:alert(1)//", "https://web.example@unlisted.example/"] {
		let response = start(client, base, SECOND, &[("redirectUrl", target)]).await?;
		let response = callback(client, base, SECOND, &response, "dave").await?;

		assert_eq!(response.status(), StatusCode::BAD_REQUEST);
	}

	Ok(())
}

/// Ask Tuwunel to start a sign-in at `idp`, answering with the redirect to
/// the provider and the grant cookie the callback requires.
async fn start(
	client: &Client,
	base: &str,
	idp: &str,
	query: &[(&str, &str)],
) -> Result<Response> {
	let url = format!("{base}/_matrix/client/v3/login/sso/redirect/{idp}");
	let response = client.get(url).query(query).send().await?;

	assert_eq!(response.status(), StatusCode::FOUND);

	Ok(response)
}

/// Return from the provider to the grant `redirect` started, signed in as
/// `sub`.
async fn callback(
	client: &Client,
	base: &str,
	idp: &str,
	redirect: &Response,
	sub: &str,
) -> Result<Response> {
	let state = parameter(&location(redirect)?, "state");
	let cookie = redirect.headers()[SET_COOKIE]
		.to_str()
		.expect("cookie header text")
		.split(';')
		.next()
		.expect("cookie pair");

	let url = format!("{base}/_matrix/client/unstable/login/sso/callback/{idp}");
	let response = client
		.get(url)
		.query(&[("code", sub), ("state", &state)])
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

	token_owner(services, &destination).await
}

/// The account the login token on a finished sign-in's destination names.
async fn token_owner(services: &Services, destination: &Url) -> Result<OwnedUserId> {
	services
		.users
		.find_from_login_token(&parameter(destination, "loginToken"))
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
/// token as the subject, so each callback names the identity it signs in as.
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
	let sub = headers[AUTHORIZATION]
		.to_str()
		.expect("bearer token")
		.trim_start_matches("Bearer ");

	Json(json!({
		"sub": sub,
		"preferred_username": sub,
	}))
}
