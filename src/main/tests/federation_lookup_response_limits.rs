#![cfg(test)]

use std::{
	convert::Infallible,
	net::TcpListener,
	path::PathBuf,
	sync::atomic::{AtomicUsize, Ordering},
	time::Duration,
};

use axum::{Router, body::Body, routing::get};
use axum_server::{from_tcp_rustls, tls_rustls::RustlsConfig};
use futures::{StreamExt, stream};
use reqwest::StatusCode;
use tokio::{spawn, time::timeout};
use tuwunel_core::{
	Result, err,
	ruma::{RoomAliasId, ServerName},
};
use tuwunel_service::Services;

use self::fixture::boot;

#[expect(
	dead_code,
	reason = "Only listener readiness is shared with the client API harness."
)]
mod client;

mod fixture;

const CERTIFICATE: &str = "../../nix/pkgs/complement/certificate.crt";

const PRIVATE_KEY: &str = "../../nix/pkgs/complement/private_key.key";

const TIMEOUT: Duration = Duration::from_secs(10);

/// Size of the start of every answer the peer sends.
const OVERSIZED: usize = 16 * 1024 * 1024;

/// Hierarchy requests the peer answered.
static HIERARCHY_ASKED: AtomicUsize = AtomicUsize::new(0);

/// Lookups a client can start without an account stop reading an oversized
/// answer.
///
/// The peer sends the start of an answer larger than any room summary,
/// alias, signing key or profile and then nothing more. Each lookup gives up
/// on it as soon as it passes the lookup's limit, rather than reading on
/// towards `max_response_size` and waiting for the rest. A room summary asks
/// a server named twice in `via` once.
#[test]
fn oversized_lookup_answers_are_dropped_while_read() -> Result {
	let options = ["ip_range_denylist=[]", "allow_invalid_tls_certificates=true"];

	boot("federation-lookup-response-limits", options, exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	let listener = TcpListener::bind(("127.0.0.1", 0))?;
	let peer = listener.local_addr()?;
	let peer_name =
		ServerName::parse(peer.to_string()).map_err(|e| err!("peer server name: {e}"))?;

	// tokio refuses to adopt a blocking socket
	listener.set_nonblocking(true)?;

	let server = spawn(serve_peer(listener));

	let not_found = async |path: String| {
		let url = format!("{base}{path}");
		let response = services.client.clients.default.get(url).send();

		matches!(
			timeout(TIMEOUT, response).await,
			Ok(Ok(response)) if response.status() == StatusCode::NOT_FOUND
		)
	};

	let summary = format!("/_matrix/client/v1/room_summary/!x:{peer}?via={peer}&via={peer}");
	let summary = not_found(summary).await;
	let profile = not_found(format!("/_matrix/client/v3/profile/@x:{peer}")).await;

	let alias = RoomAliasId::parse(format!("#x:{peer}"))?;
	let alias = timeout(TIMEOUT, services.alias.resolve_alias(&alias)).await;
	let origin_keys = timeout(TIMEOUT, services.server_keys.server_request(&peer_name)).await;
	let notary_keys = timeout(
		TIMEOUT,
		services
			.server_keys
			.notary_request(&peer_name, &peer_name),
	)
	.await;

	server.abort();

	let waited: Vec<_> = [
		("room summary", summary),
		("profile", profile),
		("alias", alias.is_ok_and(|alias| alias.is_err())),
		("origin keys", origin_keys.is_ok_and(|keys| keys.is_err())),
		("notary keys", notary_keys.is_ok_and(|keys| keys.is_err())),
	]
	.into_iter()
	.filter_map(|(lookup, gave_up)| (!gave_up).then_some(lookup))
	.collect();

	assert!(waited.is_empty(), "waited for the rest of an oversized answer: {waited:?}");
	assert_eq!(
		HIERARCHY_ASKED.load(Ordering::Relaxed),
		1,
		"the room summary did not ask its via server exactly once"
	);

	Ok(())
}

async fn serve_peer(listener: TcpListener) -> Result {
	let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
	let config =
		RustlsConfig::from_pem_file(manifest.join(CERTIFICATE), manifest.join(PRIVATE_KEY))
			.await?;

	let app = Router::new()
		.route("/_matrix/federation/v1/hierarchy/{room_id}", get(hierarchy))
		.fallback(oversized);

	from_tcp_rustls(listener, config)?
		.serve(app.into_make_service())
		.await?;

	Ok(())
}

async fn hierarchy() -> Body {
	HIERARCHY_ASKED.fetch_add(1, Ordering::Relaxed);

	oversized().await
}

async fn oversized() -> Body {
	let start = format!(r#"{{"pad":"{}"#, "x".repeat(OVERSIZED));
	let start = stream::once(async { Ok::<_, Infallible>(start) });

	Body::from_stream(start.chain(stream::pending()))
}
