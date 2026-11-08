#![cfg(test)]

use std::net::TcpListener;

use futures::future::join;
use reqwest::StatusCode;
use serde_json::{Value, json};
use tuwunel::{Args, Runtime, Server, async_run, async_start, async_stop};
use tuwunel_core::{Err, Result, ruma::UserId};
use tuwunel_service::{
	Services,
	oauth::{Session, UserInfo},
	users::PASSWORD_SENTINEL,
};

use self::client::wait_until_ready;

#[expect(
	dead_code,
	reason = "Only listener readiness is shared with the client API harness."
)]
mod client;

// The other provider's id sorts first, so preferring the default is not the
// set's own order.
const DEFAULT_IDP: &str = "uiaa-default-idp";
const OTHER_IDP: &str = "uiaa-another-idp";

/// With several providers configured, a device that signed in through the
/// legacy SSO redirect and `m.login.token` carries no provider tag. An SSO
/// account without a password must still get `m.login.sso`, bound to the
/// provider the account is linked to; otherwise it is offered no flow and can
/// never replace its cross-signing keys. A sign-in that never finished (a
/// chained provider's session is stored under the account before it
/// authenticates) is not a link.
#[test]
fn uiaa_sso_binds_the_linked_provider_of_an_untagged_device() -> Result {
	let listener = TcpListener::bind(("127.0.0.1", 0))?;
	let port = listener.local_addr()?.port();
	let provider = |name: &str, client_id: &str, default: bool| {
		[
			format!("identity_provider.{name}.client_id=\"{client_id}\""),
			format!("identity_provider.{name}.client_secret=\"test-secret\""),
			format!("identity_provider.{name}.brand=\"{name}\""),
			format!("identity_provider.{name}.issuer_url=\"https://{name}.invalid\""),
			format!("identity_provider.{name}.default={default}"),
		]
	};

	let args = provider("first", DEFAULT_IDP, true)
		.into_iter()
		.chain(provider("second", OTHER_IDP, false))
		.fold(
			Args::default_test(&["fresh", "cleanup"])
				.with_option("address=[\"127.0.0.1\"]")
				.with_option(format!("port={port}"))
				.with_option("listening=true")
				.with_option("log_global_default=false")
				.with_option("jwt.enable=false"),
			Args::with_option,
		);

	let runtime = Runtime::new(Some(&args))?;
	let server = Server::new(Some(&args), Some(&runtime))?;

	runtime.block_on(async {
		let services = async_start(&server).await?;
		let base = format!("http://127.0.0.1:{port}");

		drop(listener);

		let exercise = async {
			let outcome = exercise(&services, &base).await;
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

	let sso = |idp: &str| {
		(
			json!([{"stages": ["m.login.sso"]}]),
			json!({"m.login.sso": {"identity_providers": [{"id": idp}]}}),
		)
	};
	let no_flows = (json!([]), json!({}));

	// Each session: its provider and whether its sign-in finished.
	for (localpart, sessions, (expected_flows, expected_params)) in [
		("linked-other", &[(OTHER_IDP, true)][..], sso(OTHER_IDP)),
		("linked-both", &[(OTHER_IDP, true), (DEFAULT_IDP, true)][..], sso(DEFAULT_IDP)),
		("linked-pending", &[(OTHER_IDP, true), (DEFAULT_IDP, false)][..], sso(OTHER_IDP)),
		("linked-gone", &[("uiaa-removed-idp", true)][..], no_flows.clone()),
		("unlinked", &[][..], no_flows.clone()),
	] {
		let user_id = UserId::parse_with_server_name(localpart, services.globals.server_name())?;
		services
			.users
			.create(&user_id, Some(PASSWORD_SENTINEL), Some("sso"))
			.await?;

		for (index, &(idp_id, finished)) in sessions.iter().enumerate() {
			let user_info = UserInfo {
				sub: format!("uiaa-linked-sub-{localpart}-{index}"),
				..Default::default()
			};
			let session = Session {
				idp_id: Some(idp_id.to_owned()),
				sess_id: Some(format!("uiaa-linked-session-{localpart}-{index}")),
				user_id: Some(user_id.clone()),
				user_info: finished.then_some(user_info),
				..Default::default()
			};

			services.oauth.sessions.put(&session).await;
		}

		// A device from the token login, which records no provider for it.
		let token = format!("uiaa-sso-linked-test-access-token-{localpart}");
		services
			.users
			.create_device(&user_id, None, (Some(&token), None), None, None, None)
			.await?;

		let response = services
			.client
			.clients
			.default
			.post(format!("{base}/_matrix/client/v3/account/deactivate"))
			.bearer_auth(&token)
			.json(&json!({}))
			.send()
			.await?;

		if response.status() != StatusCode::UNAUTHORIZED {
			return Err!("{localpart}: expected a UIAA challenge, got {}", response.status());
		}

		let challenge: Value = response.json().await?;
		if challenge["flows"] != expected_flows {
			return Err!(
				"{localpart}: expected flows {expected_flows}, got {}",
				challenge["flows"]
			);
		}

		if challenge["params"] != expected_params {
			return Err!(
				"{localpart}: expected params {expected_params}, got {}",
				challenge["params"]
			);
		}
	}

	Ok(())
}
