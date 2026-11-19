#![cfg(test)]

use std::{
	borrow::Borrow,
	iter::once,
	net::TcpListener,
	path::PathBuf,
	sync::{Arc, Mutex},
};

use axum::{Json, Router, routing::post};
use axum_server::{from_tcp_rustls, tls_rustls::RustlsConfig};
use serde_json::{Map, Value, from_value, json};
use tokio::{spawn, task::JoinHandle};
use tuwunel_core::{
	Result, err,
	ruma::{
		MilliSecondsSinceUnixEpoch, OwnedServerSigningKeyId,
		api::federation::discovery::ServerSigningKeys, server_name,
	},
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

/// Padding keys in a notary document: about 35 KiB, so one document fits the
/// 64 KiB stored row on its own and two together do not.
const PADDING: usize = 500;

/// The stored row's size limit.
const ROW_MAX: usize = 64 * 1024;

/// A notary answering each requested retired key with its own document,
/// padded by the given number of keys.
#[derive(Clone)]
struct Notary {
	padding: fn(usize) -> usize,
	requested: Arc<Mutex<Vec<Vec<String>>>>,
}

/// One notary batch keeps both retired keys it acquired.
///
/// The notary answers with two documents for one server, each holding one of
/// the two requested retired keys. Storing the second must not drop the key
/// the first brought, because the acquisition then counts both as cached.
#[test]
fn notary_batch_keeps_every_key_it_acquired() -> Result {
	let notary = Notary::new(|_| PADDING);

	run("federation-notary-key-batch", [&notary], async |services| {
		acquire(services, &[1, 2]).await;
		assert_cached(services, &[1, 2]).await
	})
}

/// A notary batch keeps a requested key it found cached.
///
/// The first acquisition caches `retired1` with padding. The second asks for
/// both retired keys, so only `retired2` is fetched; storing its document must
/// not drop the cached `retired1` the same batch relies on.
#[test]
fn notary_batch_keeps_a_requested_key_found_cached() -> Result {
	let notary = Notary::new(|_| PADDING);

	run("federation-notary-key-cached", [&notary], async |services| {
		acquire(services, &[1]).await;
		acquire(services, &[1, 2]).await;
		assert_eq!(notary.requested(), [vec![retired_id(1)], vec![retired_id(2)]]);
		assert_cached(services, &[1, 2]).await
	})
}

/// A document that fills the row on its own drops an earlier key of the
/// batch, which is then missing and asked of the next notary.
///
/// The first notary's second document is just under 64 KiB serialized, so
/// only it fits the row and `retired1` from its first document is dropped.
/// The second notary is then asked for `retired1`, and its smaller document
/// fits next to `retired2`.
#[test]
fn notary_batch_requests_a_key_the_row_dropped() -> Result {
	let first = Notary::new(|retired| if retired == 2 { filling_padding() } else { PADDING });
	let second = Notary::new(|_| PADDING);

	run("federation-notary-key-fallback", [&first, &second], async |services| {
		acquire(services, &[1, 2]).await;
		assert_eq!(first.requested(), [vec![retired_id(1), retired_id(2)]]);
		assert_eq!(second.requested(), [vec![retired_id(1)]]);
		assert_cached(services, &[1, 2]).await
	})
}

fn run<const N: usize>(
	name: &str,
	notaries: [&Notary; N],
	exercise: impl AsyncFnOnce(&Services) -> Result,
) -> Result {
	let listeners = notaries.map(|_| TcpListener::bind(("127.0.0.1", 0)));
	let listeners: Vec<_> = listeners.into_iter().collect::<Result<_, _>>()?;
	let trusted: Vec<_> = listeners
		.iter()
		.map(|listener| Ok(format!("\"{}\"", listener.local_addr()?)))
		.collect::<Result<_>>()?;

	let trusted = format!("federation.keys.trusted_servers=[{}]", trusted.join(","));
	let options = [
		"logging.enable=false",
		"ip_range_denylist=[]",
		"allow_invalid_tls_certificates=true",
		"federation.keys.query_trusted_servers_first=true",
		"federation.keys.only_query_trusted_servers=true",
		trusted.as_str(),
	];

	boot(name, options, async |services, _| {
		let servers = listeners
			.into_iter()
			.zip(notaries)
			.map(|(listener, notary)| notary.clone().serve(listener))
			.collect::<Result<Vec<_>>>()?;

		let outcome = exercise(services).await;
		servers.iter().for_each(JoinHandle::abort);

		outcome
	})
}

async fn acquire(services: &Services, retired: &[usize]) {
	let remote = server_name!("remote.test");
	let key_ids: Vec<OwnedServerSigningKeyId> = retired
		.iter()
		.map(|&retired| retired_id(retired).try_into())
		.collect::<Result<_, _>>()
		.expect("valid key ids");

	services
		.server_keys
		.acquire_pubkeys(once((remote, key_ids.iter().map(Borrow::borrow))))
		.await;
}

async fn assert_cached(services: &Services, retired: &[usize]) -> Result {
	let remote = server_name!("remote.test");
	for &retired in retired {
		let key_id: OwnedServerSigningKeyId = retired_id(retired).try_into()?;
		assert!(
			services
				.server_keys
				.verify_key_exists(remote, &key_id)
				.await,
			"{key_id} acquired in the batch is not cached"
		);
	}

	Ok(())
}

impl Notary {
	fn new(padding: fn(usize) -> usize) -> Self { Self { padding, requested: Arc::default() } }

	/// The retired key IDs of each request, in order.
	fn requested(&self) -> Vec<Vec<String>> {
		self.requested
			.lock()
			.expect("requests lock")
			.clone()
	}

	fn serve(self, listener: TcpListener) -> Result<JoinHandle<Result>> {
		// tokio refuses to adopt a blocking socket
		listener.set_nonblocking(true)?;

		Ok(spawn(async move {
			let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
			let config = RustlsConfig::from_pem_file(
				manifest.join(CERTIFICATE),
				manifest.join(PRIVATE_KEY),
			)
			.await?;

			let app = Router::new().route(
				"/_matrix/key/v2/query",
				post(async move |Json(body): Json<Value>| self.query(&body)),
			);

			from_tcp_rustls(listener, config)?
				.serve(app.into_make_service())
				.await?;

			Ok(())
		}))
	}

	fn query(&self, body: &Value) -> Json<Value> {
		let key_ids: Vec<String> = body["server_keys"][REMOTE]
			.as_object()
			.map(|keys| keys.keys().cloned().collect())
			.unwrap_or_default();

		let documents: Vec<_> = key_ids
			.iter()
			.filter_map(|key_id| key_id.strip_prefix("ed25519:retired"))
			.filter_map(|retired| retired.parse().ok())
			.map(|retired| document(retired, (self.padding)(retired)))
			.collect();

		self.requested
			.lock()
			.expect("requests lock")
			.push(key_ids);

		Json(json!({ "server_keys": documents }))
	}
}

fn document(retired: usize, padding: usize) -> Value {
	let now = MilliSecondsSinceUnixEpoch::now();
	let verify_keys: Map<String, Value> = (0..padding)
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

/// The most padding keys for which the second document still fits the row on
/// its own, checked to leave too little room for `retired1` beside it.
fn filling_padding() -> usize {
	let stored = |padding| stored_size(document(2, padding)).expect("document serializes");
	let padding = (PADDING..ROW_MAX)
		.take_while(|&padding| stored(padding) <= ROW_MAX)
		.last()
		.expect("one padding fits");

	let mut beside = document(2, padding);
	beside["old_verify_keys"][retired_id(1)] =
		document(1, 0)["old_verify_keys"][retired_id(1)].clone();
	assert!(stored_size(beside).expect("document serializes") > ROW_MAX);

	padding
}

/// The size of a document as the stored row: a cached row keeps no signatures,
/// and none are sent here, so the stored form is the document itself.
fn stored_size(document: Value) -> Result<usize> {
	let keys: ServerSigningKeys = from_value(document)?;

	serde_json::to_vec(&keys)
		.map(|json| json.len())
		.map_err(|e| err!("{e}"))
}

fn retired_id(retired: usize) -> String { format!("ed25519:retired{retired}") }
