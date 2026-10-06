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
use tuwunel_service::Services;

use self::client::wait_until_ready;

const IDP: &str = "test-idp";
const TRUSTED: &str = "https://trusted.example/callback";

#[test]
fn sso_redirect_confirmation() -> Result {
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
		.with_option("oidc_registration_allowed_redirect_hosts=[\"trusted.example\"]")
		.with_option("sso_trusted_redirect_hosts=[\"web.example\", \"exampleapp\"]")
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

/// A login token goes straight only to this server or a listed host. Any other
/// target is named on a page, and only following its link delivers the token.
async fn exercise(services: &Services, base: &str) -> Result {
	wait_until_ready(services, base).await?;

	let client = Client::builder()
		.redirect(Policy::none())
		.timeout(Duration::from_secs(10))
		.build()?;

	let own = format!("{base}/_tuwunel/oidc/_complete?oidc_req_id=x");
	let carol = UserId::parse_with_server_name("carol", services.globals.server_name())?;

	for (target, asks) in [
		(TRUSTED, false),
		(own.as_str(), false),
		("https://web.example/login", false),
		("exampleapp://auth/login", false),
		("https://unlisted.example/", true),
		("element://connect", true),
	] {
		let response = sign_in(&client, base, target, "carol").await?;

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
		assert_eq!(token_owner(services, &destination).await?, carol);
	}

	for target in ["javascript:alert(1)//", "https://web.example@unlisted.example/"] {
		let response = sign_in(&client, base, target, "carol").await?;

		assert_eq!(response.status(), StatusCode::BAD_REQUEST);
	}

	Ok(())
}

/// Sign in as `sub` with `redirectUrl` naming `target`, answering with the
/// callback's response.
async fn sign_in(client: &Client, base: &str, target: &str, sub: &str) -> Result<Response> {
	let url = format!("{base}/_matrix/client/v3/login/sso/redirect/{IDP}");
	let redirect = client
		.get(url)
		.query(&[("redirectUrl", target)])
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
		.query(&[("code", sub), ("state", &state)])
		.header(COOKIE, cookie)
		.send()
		.await?;

	Ok(response)
}

/// The account the login token on a finished sign-in's destination names.
async fn token_owner(services: &Services, destination: &Url) -> Result<OwnedUserId> {
	services
		.users
		.find_from_login_token(&parameter(destination, "loginToken"))
		.await
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
