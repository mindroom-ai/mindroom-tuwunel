#![cfg(test)]

use std::{
	net::{SocketAddr, TcpListener},
	path::PathBuf,
	sync::atomic::{AtomicUsize, Ordering},
};

use axum::{Router, extract::State, response::Redirect, routing::any};
use axum_server::{from_tcp, from_tcp_rustls, tls_rustls::RustlsConfig};
use tokio::spawn;
use tuwunel_core::Result;
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

/// Requests the peer answered with a redirect.
static ASKED: AtomicUsize = AtomicUsize::new(0);

/// Requests that reached the address the peer redirects to.
static REACHED: AtomicUsize = AtomicUsize::new(0);

/// Server discovery follows a peer's redirects only to HTTPS URLs.
///
/// The peer moves its well-known document to another HTTPS path, which is
/// followed, and from there redirects to a plain HTTP address of its choosing.
/// Following that hop would continue the lookup without TLS at an address no
/// one checked, so the lookup fails without contacting it.
#[test]
fn well_known_does_not_follow_a_redirect_to_plain_http() -> Result {
	let options = ["ip_range_denylist=[]", "allow_invalid_tls_certificates=true"];

	boot("well-known-redirect", options, exercise)
}

async fn exercise(services: &Services, _base: &str) -> Result {
	let target = TcpListener::bind(("127.0.0.1", 0))?;
	let target_address = target.local_addr()?;
	let peer = TcpListener::bind(("127.0.0.1", 0))?;
	let peer_address = peer.local_addr()?;

	// tokio refuses to adopt a blocking socket
	target.set_nonblocking(true)?;
	peer.set_nonblocking(true)?;

	let target = spawn(serve_target(target));
	let peer = spawn(serve_peer(peer, target_address));

	let response = services
		.client
		.well_known
		.get(format!("https://{peer_address}/.well-known/matrix/server"))
		.send()
		.await;

	target.abort();
	peer.abort();

	assert_eq!(ASKED.load(Ordering::Relaxed), 2, "the HTTPS redirect was not followed");
	assert!(response.is_err(), "the plain HTTP redirect was followed");
	assert_eq!(REACHED.load(Ordering::Relaxed), 0, "the plain HTTP address was contacted");

	Ok(())
}

async fn serve_peer(listener: TcpListener, target: SocketAddr) -> Result {
	let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
	let config =
		RustlsConfig::from_pem_file(manifest.join(CERTIFICATE), manifest.join(PRIVATE_KEY))
			.await?;

	let app = Router::new()
		.route("/.well-known/matrix/server", any(moved))
		.fallback(any(redirect))
		.with_state(target);

	from_tcp_rustls(listener, config)?
		.serve(app.into_make_service())
		.await?;

	Ok(())
}

async fn moved() -> Redirect {
	ASKED.fetch_add(1, Ordering::Relaxed);

	Redirect::temporary("/moved")
}

async fn redirect(State(target): State<SocketAddr>) -> Redirect {
	ASKED.fetch_add(1, Ordering::Relaxed);

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
