#![cfg(test)]

use std::net::TcpListener;

use tuwunel::{Args, Runtime, Server, async_run, async_start, async_stop};
use tuwunel_core::{
	Result, err,
	ruma::api::appservice::{
		Namespaces, Registration, RegistrationInit, ping::send_ping, thirdparty::get_protocol,
	},
};
use tuwunel_service::Services;

const HS_TOKEN: &str = "appservice-request-error-hs-token";

/// Requests to an unreachable appservice fail without its `hs_token`.
///
/// The token also travels as the request URL's `access_token` query, and the
/// failed request's error is logged, so that error must not name the URL.
#[test]
fn unreachable_appservice_errors_omit_hs_token() -> Result {
	let mut args = Args::default_test(&["fresh", "cleanup"]);
	args.maintenance = true;

	let runtime = Runtime::new(Some(&args))?;
	let server = Server::new(Some(&args), Some(&runtime))?;
	let result: Result = runtime.block_on(async {
		let services = async_start(&server).await?;

		let outcome = exercise(&services).await;

		server.server.shutdown()?;
		drop(services);

		async_run(&server).await?;
		async_stop(&server).await?;

		outcome
	});

	drop(runtime);

	result
}

async fn exercise(services: &Services) -> Result {
	// Nothing listens on the port once the listener closes.
	let url = {
		let listener = TcpListener::bind(("127.0.0.1", 0))?;
		format!("http://{}", listener.local_addr()?)
	};

	let registration: Registration = RegistrationInit {
		id: "unreachable".to_owned(),
		url: Some(url),
		as_token: "appservice-request-error-as-token".to_owned(),
		hs_token: HS_TOKEN.to_owned(),
		sender_localpart: "unreachable".to_owned(),
		namespaces: Namespaces::new(),
		rate_limited: None,
		protocols: None,
	}
	.into();

	let request = get_protocol::v1::Request::new("irc".to_owned());
	let error = services
		.appservice
		.send_request(registration.clone(), request)
		.await
		.err()
		.ok_or_else(|| err!("request to a closed port succeeded"))?;

	assert!(
		!format!("{error:?}").contains(HS_TOKEN),
		"request error names the hs_token: {error:?}"
	);

	let error = services
		.appservice
		.ping(registration, send_ping::v1::Request::new())
		.await
		.err()
		.ok_or_else(|| err!("ping to a closed port succeeded"))?;

	assert!(
		!format!("{error:?}").contains(HS_TOKEN),
		"ping error names the hs_token: {error:?}"
	);

	Ok(())
}
