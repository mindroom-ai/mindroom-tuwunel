#![cfg(test)]

use std::{
	convert::Infallible,
	net::TcpListener,
	num::NonZeroUsize,
	path::PathBuf,
	sync::atomic::{AtomicUsize, Ordering},
	time::Duration,
};

use axum::{Router, body::Body, routing::get};
use axum_server::{from_tcp_rustls, tls_rustls::RustlsConfig};
use futures::{StreamExt, stream};
use tokio::{spawn, time::timeout};
use tuwunel_core::{
	Result, err,
	matrix::pdu::MAX_PDU_BYTES,
	ruma::{EventId, ServerName},
};
use tuwunel_service::{
	Services,
	fetcher::{Op, Opts},
};

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

/// Requests the peer answered.
static ASKED: AtomicUsize = AtomicUsize::new(0);

/// An event fetch stops reading a response once it is too large for an event.
///
/// The peer sends the start of an `/event` answer far larger than any event
/// and then nothing more. The fetch gives up on it as soon as it passes the
/// event response limit, rather than reading on towards `max_response_size`
/// and waiting for the rest.
#[test]
fn oversized_event_response_is_dropped_while_read() -> Result {
	let options = ["ip_range_denylist=[]", "allow_invalid_tls_certificates=true"];

	boot("federation-event-response-limit", options, exercise)
}

async fn exercise(services: &Services, _base: &str) -> Result {
	let listener = TcpListener::bind(("127.0.0.1", 0))?;
	let peer_name = ServerName::parse(listener.local_addr()?.to_string())
		.map_err(|e| err!("peer server name: {e}"))?;

	// tokio refuses to adopt a blocking socket
	listener.set_nonblocking(true)?;

	let peer = spawn(serve_peer(listener));

	let opts = Opts::unscoped(Op::Event)
		.event_id(EventId::parse("$federation-event-response-limit")?)
		.hint(peer_name)
		.attempt_limit(NonZeroUsize::MIN);

	let fetched = timeout(TIMEOUT, services.fetcher.fetch(opts)).await;

	peer.abort();

	assert_eq!(ASKED.load(Ordering::Relaxed), 1, "the peer was not asked");
	assert!(fetched.is_ok(), "the fetch waited for the rest of an oversized response");
	assert!(fetched.is_ok_and(|fetched| fetched.is_err()), "an oversized event was accepted");

	Ok(())
}

async fn serve_peer(listener: TcpListener) -> Result {
	let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
	let config =
		RustlsConfig::from_pem_file(manifest.join(CERTIFICATE), manifest.join(PRIVATE_KEY))
			.await?;

	let app = Router::new().route("/_matrix/federation/v1/event/{event_id}", get(event));

	from_tcp_rustls(listener, config)?
		.serve(app.into_make_service())
		.await?;

	Ok(())
}

async fn event() -> Body {
	ASKED.fetch_add(1, Ordering::Relaxed);

	let start = format!(
		r#"{{"origin":"peer","origin_server_ts":0,"pdus":[{{"pad":"{}"#,
		"x".repeat(8 * MAX_PDU_BYTES)
	);

	let start = stream::once(async { Ok::<_, Infallible>(start) });

	Body::from_stream(start.chain(stream::pending()))
}
