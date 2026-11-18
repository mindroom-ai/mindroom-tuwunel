#![cfg(test)]

use std::{borrow::Borrow, iter::once, net::TcpListener, path::PathBuf};

use axum::{Json, Router, routing::post};
use axum_server::{from_tcp_rustls, tls_rustls::RustlsConfig};
use serde_json::{Map, Value, json};
use tokio::spawn;
use tuwunel_core::{
	Result,
	ruma::{MilliSecondsSinceUnixEpoch, OwnedServerSigningKeyId, server_name},
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

/// An ed25519 public key of 32 zero bytes.
const KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

/// The server whose retired keys the batch asks the notary for.
const REMOTE: &str = "remote.test";

/// Padding keys in each notary document: about 35 KiB, so each document fits
/// the 64 KiB stored row on its own and the two together do not.
const PADDING: usize = 500;

/// One notary batch keeps both retired keys it acquired.
///
/// The notary answers with two documents for one server, each holding one of
/// the two requested retired keys. Storing the second must not drop the key
/// the first brought, because the acquisition then counts both as cached.
#[test]
fn notary_batch_keeps_every_key_it_acquired() -> Result {
	let listener = TcpListener::bind(("127.0.0.1", 0))?;
	let notary = listener.local_addr()?.to_string();

	// tokio refuses to adopt a blocking socket
	listener.set_nonblocking(true)?;

	let trusted = format!("federation.keys.trusted_servers=[\"{notary}\"]");
	let options = [
		"ip_range_denylist=[]",
		"allow_invalid_tls_certificates=true",
		"federation.keys.query_trusted_servers_first=true",
		"federation.keys.only_query_trusted_servers=true",
		trusted.as_str(),
	];

	boot("federation-notary-key-batch", options, async |services, _| {
		exercise(services, listener).await
	})
}

async fn exercise(services: &Services, listener: TcpListener) -> Result {
	let server = spawn(serve_notary(listener));
	let remote = server_name!("remote.test");
	let retired: [OwnedServerSigningKeyId; 2] =
		[retired_id(1).try_into()?, retired_id(2).try_into()?];

	services
		.server_keys
		.acquire_pubkeys(once((remote, retired.iter().map(Borrow::borrow))))
		.await;

	server.abort();

	for key_id in &retired {
		assert!(
			services
				.server_keys
				.verify_key_exists(remote, key_id)
				.await,
			"{key_id} acquired in the batch is not cached"
		);
	}

	Ok(())
}

async fn serve_notary(listener: TcpListener) -> Result {
	let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
	let config =
		RustlsConfig::from_pem_file(manifest.join(CERTIFICATE), manifest.join(PRIVATE_KEY))
			.await?;

	let app = Router::new().route("/_matrix/key/v2/query", post(query));

	from_tcp_rustls(listener, config)?
		.serve(app.into_make_service())
		.await?;

	Ok(())
}

/// Two documents for the remote server, each with one retired key.
async fn query() -> Json<Value> { Json(json!({ "server_keys": [document(1), document(2)] })) }

fn document(retired: usize) -> Value {
	let now = MilliSecondsSinceUnixEpoch::now();
	let verify_keys: Map<String, Value> = (0..PADDING)
		.map(|id| (format!("ed25519:pad{retired}x{id}"), json!({ "key": KEY })))
		.collect();

	json!({
		"server_name": REMOTE,
		"verify_keys": verify_keys,
		"old_verify_keys": {
			(retired_id(retired)): { "key": KEY, "expired_ts": now },
		},
		"signatures": {},
		"valid_until_ts": now,
	})
}

fn retired_id(retired: usize) -> String { format!("ed25519:retired{retired}") }
