#![cfg(test)]

use std::{
	convert::Infallible,
	net::TcpListener,
	num::NonZeroUsize,
	path::PathBuf,
	sync::atomic::{AtomicUsize, Ordering},
	time::Duration,
};

use axum::{
	Router,
	body::Body,
	routing::{get, post},
};
use axum_server::{from_tcp_rustls, tls_rustls::RustlsConfig};
use futures::{StreamExt, stream};
use tokio::{spawn, time::timeout};
use tuwunel_core::{
	Result, err,
	matrix::pdu::MAX_PDU_BYTES,
	ruma::{EventId, RoomId, ServerName},
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

/// Event fetches stop reading a response once it is too large for what they
/// asked for.
///
/// The peer sends the start of an `/event` and a `/get_missing_events` answer
/// far larger than any it could serve and then nothing more. Each fetch gives
/// up on it as soon as it passes the limit for its request, rather than reading
/// on towards `max_response_size` and waiting for the rest.
#[test]
fn oversized_fetch_responses_are_dropped_while_read() -> Result {
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

	let event_id = EventId::parse("$federation-event-response-limit")?;
	let room_id = RoomId::parse(format!("!federation-event-response-limit:{peer_name}"))?;
	let gave_up = async |opts: Opts| {
		let opts = opts
			.hint(peer_name.clone())
			.attempt_limit(NonZeroUsize::MIN);

		matches!(timeout(TIMEOUT, services.fetcher.fetch(opts)).await, Ok(Err(_)))
	};

	let event = gave_up(Opts::unscoped(Op::Event).event_id(event_id.clone())).await;
	let missing_events = Opts::new(Op::MissingEvents, room_id).latest_events([event_id]);
	let missing_events = gave_up(missing_events).await;

	peer.abort();

	let waited: Vec<_> = [("event", event), ("missing events", missing_events)]
		.into_iter()
		.filter_map(|(fetch, gave_up)| (!gave_up).then_some(fetch))
		.collect();

	assert_eq!(ASKED.load(Ordering::Relaxed), 2, "the peer was not asked for each fetch");
	assert!(waited.is_empty(), "waited for the rest of an oversized response: {waited:?}");

	Ok(())
}

async fn serve_peer(listener: TcpListener) -> Result {
	let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
	let config =
		RustlsConfig::from_pem_file(manifest.join(CERTIFICATE), manifest.join(PRIVATE_KEY))
			.await?;

	// Each answer starts past its request's limit: larger than one served event,
	// and than the ten served events a missing-events batch asks for.
	let app = Router::new()
		.route(
			"/_matrix/federation/v1/event/{event_id}",
			get(async || oversized(8 * MAX_PDU_BYTES)),
		)
		.route(
			"/_matrix/federation/v1/get_missing_events/{room_id}",
			post(async || oversized(64 * MAX_PDU_BYTES)),
		);

	from_tcp_rustls(listener, config)?
		.serve(app.into_make_service())
		.await?;

	Ok(())
}

fn oversized(len: usize) -> Body {
	ASKED.fetch_add(1, Ordering::Relaxed);

	let start = format!(r#"{{"pad":"{}"#, "x".repeat(len));
	let start = stream::once(async { Ok::<_, Infallible>(start) });

	Body::from_stream(start.chain(stream::pending()))
}
