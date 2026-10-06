#![cfg(test)]

use std::{
	net::{SocketAddr, TcpListener},
	path::PathBuf,
	sync::atomic::{AtomicBool, AtomicUsize, Ordering},
	time::Duration,
};

use axum::{
	Router,
	extract::{RawQuery, State},
	response::Redirect,
	routing::any,
};
use axum_server::{from_tcp, from_tcp_rustls, tls_rustls::RustlsConfig};
use tokio::spawn;
use tuwunel_core::{
	Result, err,
	ruma::{Mxc, ServerName},
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

/// Requests the peer answered with a redirect.
static ASKED: AtomicUsize = AtomicUsize::new(0);

/// Whether a request told the peer it may redirect.
static REDIRECT_ALLOWED: AtomicBool = AtomicBool::new(false);

/// Requests that reached the address the peer redirects to.
static REACHED: AtomicUsize = AtomicUsize::new(0);

/// A peer's redirect is not followed by a legacy media fetch.
///
/// The peer answers with a redirect to a plain HTTP address of its choosing,
/// whatever the request asked for. Following it would fetch from an address no
/// one checked against `ip_range_denylist` and store its answer as the peer's
/// media, so the fetch fails without contacting that address.
#[test]
fn legacy_media_does_not_follow_a_peers_redirect() -> Result {
	let options = ["ip_range_denylist=[]", "allow_invalid_tls_certificates=true"];

	boot("federation-redirect", options, exercise)
}

async fn exercise(services: &Services, _base: &str) -> Result {
	let target = TcpListener::bind(("127.0.0.1", 0))?;
	let target_address = target.local_addr()?;
	let peer = TcpListener::bind(("127.0.0.1", 0))?;
	let peer_name = ServerName::parse(peer.local_addr()?.to_string())
		.map_err(|e| err!("peer server name: {e}"))?;

	// tokio refuses to adopt a blocking socket
	target.set_nonblocking(true)?;
	peer.set_nonblocking(true)?;

	let target = spawn(serve_target(target));
	let peer = spawn(serve_peer(peer, target_address));

	let mxc = Mxc {
		server_name: &peer_name,
		media_id: "redirectedlegacymedia",
	};

	let fetched = services
		.media
		.fetch_remote_content_legacy(&mxc, TIMEOUT)
		.await;

	target.abort();
	peer.abort();

	assert_eq!(ASKED.load(Ordering::Relaxed), 1, "the peer was not asked");
	assert!(!REDIRECT_ALLOWED.load(Ordering::Relaxed), "the request allowed a redirect");
	assert!(fetched.is_err(), "the redirect target was stored as the peer's media");
	assert_eq!(REACHED.load(Ordering::Relaxed), 0, "the redirect was followed");

	Ok(())
}

async fn serve_peer(listener: TcpListener, target: SocketAddr) -> Result {
	let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
	let config =
		RustlsConfig::from_pem_file(manifest.join(CERTIFICATE), manifest.join(PRIVATE_KEY))
			.await?;

	let app = Router::new()
		.fallback(any(redirect))
		.with_state(target);

	from_tcp_rustls(listener, config)?
		.serve(app.into_make_service())
		.await?;

	Ok(())
}

async fn redirect(State(target): State<SocketAddr>, RawQuery(query): RawQuery) -> Redirect {
	ASKED.fetch_add(1, Ordering::Relaxed);

	if query.is_some_and(|query| query.contains("allow_redirect=true")) {
		REDIRECT_ALLOWED.store(true, Ordering::Relaxed);
	}

	Redirect::temporary(&format!("http://{target}/internal"))
}

async fn serve_target(listener: TcpListener) -> Result {
	let app = Router::new().fallback(any(internal));

	from_tcp(listener)?
		.serve(app.into_make_service())
		.await?;

	Ok(())
}

async fn internal() -> &'static str {
	REACHED.fetch_add(1, Ordering::Relaxed);

	"internal"
}
