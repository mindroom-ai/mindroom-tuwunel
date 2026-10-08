#![cfg(test)]

use std::{
	net::TcpListener,
	path::PathBuf,
	sync::atomic::{AtomicUsize, Ordering},
};

use axum::{Json, Router, extract::State, routing::get};
use axum_server::{from_tcp_rustls, tls_rustls::RustlsConfig};
use serde_json::{Map, Value, json};
use tokio::spawn;
use tuwunel_core::{Result, ruma::MilliSecondsSinceUnixEpoch};
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

/// An ed25519 public key of 32 zero bytes.
const KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

/// Key documents the peer served.
static KEYS_ASKED: AtomicUsize = AtomicUsize::new(0);

/// A federation request looks up its origin's key once.
///
/// The peer's key document is too large to be stored, so a second lookup
/// would fetch it again.
#[test]
fn federation_request_fetches_its_origin_key_once() -> Result {
	let options = [
		"ip_range_denylist=[]",
		"allow_invalid_tls_certificates=true",
		"trusted_servers=[]",
	];

	boot("federation-origin-key-fetch", options, exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	let listener = TcpListener::bind(("127.0.0.1", 0))?;
	let peer = listener.local_addr()?.to_string();

	// tokio refuses to adopt a blocking socket
	listener.set_nonblocking(true)?;

	let server = spawn(serve_peer(listener, peer.clone()));
	let authorization = format!(
		r#"X-Matrix origin="{peer}",destination="{}",key="ed25519:a",sig="AAAA""#,
		services.globals.server_name(),
	);

	let response = services
		.client
		.clients
		.default
		.put(format!("{base}/_matrix/federation/v1/send/origin-key-fetch"))
		.header("Authorization", authorization)
		.json(&json!({ "pdus": [] }))
		.send()
		.await;

	server.abort();

	let status = response?.status().as_u16();

	assert_eq!(status, 403, "the bogus signature was not refused");
	assert_eq!(KEYS_ASKED.load(Ordering::Relaxed), 1, "the origin key was not fetched once");

	Ok(())
}

async fn serve_peer(listener: TcpListener, peer: String) -> Result {
	let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
	let config =
		RustlsConfig::from_pem_file(manifest.join(CERTIFICATE), manifest.join(PRIVATE_KEY))
			.await?;

	let app = Router::new()
		.route("/_matrix/key/v2/server", get(keys))
		.with_state(peer);

	from_tcp_rustls(listener, config)?
		.serve(app.into_make_service())
		.await?;

	Ok(())
}

/// A key document too large to store, holding the key the request names.
async fn keys(State(peer): State<String>) -> Json<Value> {
	KEYS_ASKED.fetch_add(1, Ordering::Relaxed);

	let verify_keys: Map<String, Value> = (0..2000)
		.map(|id| format!("ed25519:{id}"))
		.chain(["ed25519:a".to_owned()])
		.map(|id| (id, json!({ "key": KEY })))
		.collect();

	Json(json!({
		"server_name": peer,
		"verify_keys": verify_keys,
		"signatures": {},
		"valid_until_ts": MilliSecondsSinceUnixEpoch::now(),
	}))
}
