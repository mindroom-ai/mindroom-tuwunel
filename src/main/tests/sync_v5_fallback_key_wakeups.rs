#![cfg(test)]

use std::{
	net::TcpListener,
	time::{Duration, Instant},
};

use futures::future::join;
use serde_json::{Value, json};
use tuwunel::{Args, Runtime, Server, async_run, async_start, async_stop};
use tuwunel_core::{Result, err, implement, utils::BoolExt};
use tuwunel_service::Services;

use self::client::{Client, field, register, wait_until_ready};

mod client;

const READER_TOKEN: &str = "sync-v5-fallback-key-wakeups-reader-token";
const OTHER_TOKEN: &str = "sync-v5-fallback-key-wakeups-other-token";

/// How long a resumed sync polls for, in milliseconds.
const POLL_TIMEOUT: u64 = 1_500;

/// Drives a long poll carrying the e2ee extension past unrelated activity.
///
/// The extension reports the device's unused fallback key types on every
/// pass. Activity elsewhere on the server moves the global count, so the
/// poll's first pass runs; with nothing new for the reader it must keep
/// waiting rather than answer with the fallback key types alone.
#[test]
fn fallback_key_types_alone_do_not_end_a_long_poll() -> Result {
	let listener = TcpListener::bind(("127.0.0.1", 0))?;
	let port = listener.local_addr()?.port();

	let args = Args::default_test(&["fresh", "cleanup"])
		.with_option("address=[\"127.0.0.1\"]")
		.with_option(format!("port={port}"))
		.with_option("listening=true");

	let runtime = Runtime::new(Some(&args))?;
	let server = Server::new(Some(&args), Some(&runtime))?;
	let result = runtime.block_on(async {
		let services = async_start(&server).await?;
		let base = format!("http://127.0.0.1:{port}");

		drop(listener);

		let driven = async {
			let outcome = exercise(&services, &base).await;
			let shutdown = server.server.shutdown();

			outcome.and(shutdown)
		};

		let (served, outcome) = join(async_run(&server), driven).await;

		drop(services);
		async_stop(&server).await?;
		served?;

		outcome
	});

	drop(runtime);

	result
}

async fn exercise(services: &Services, base: &str) -> Result {
	wait_until_ready(services, base).await?;

	register(services, "fallbackreader", READER_TOKEN).await?;
	register(services, "fallbackother", OTHER_TOKEN).await?;

	let reader = Client { services, base, token: READER_TOKEN };
	let other = Client { services, base, token: OTHER_TOKEN };

	let opening = reader.sync_e2ee(None).await?;
	let pos = field(&opening, "pos")?;

	// A room the reader is not in: it moves the global count and nothing else.
	other
		.create_room(&json!({ "preset": "private_chat" }))
		.await?;

	let started = Instant::now();
	let idle = reader.sync_e2ee(Some(pos)).await?;
	let waited = started.elapsed();

	idle["extensions"]["e2ee"]["device_unused_fallback_key_types"]
		.is_array()
		.ok_or_else(|| err!("the idle round did not report the fallback key types"))?;

	waited
		.ge(&Duration::from_millis(POLL_TIMEOUT - 100))
		.ok_or_else(|| {
			err!("the fallback key types alone ended the long poll after {waited:?}")
		})?;

	let pos = field(&idle, "pos")?;
	let room = reader
		.create_room(&json!({ "preset": "private_chat" }))
		.await?;

	let started = Instant::now();
	let joined = reader.sync_e2ee(Some(pos)).await?;
	let waited = started.elapsed();

	joined["rooms"][room.as_str()]
		.is_object()
		.ok_or_else(|| err!("the round after joining did not carry the new room"))?;

	waited
		.lt(&Duration::from_millis(POLL_TIMEOUT))
		.ok_or_else(|| err!("a round with news waited out the poll ({waited:?})"))?;

	Ok(())
}

/// One sliding sync carrying the e2ee extension, optionally resuming a pos.
#[implement(Client, params = "<'_>")]
async fn sync_e2ee(&self, pos: Option<&str>) -> Result<Value> {
	let body = json!({
		"lists": {
			"all": {
				"ranges": [[0, 9]],
				"timeline_limit": 0,
			},
		},
		"extensions": {
			"e2ee": { "enabled": true },
		},
	});

	let query = pos.map_or_else(String::new, |pos| format!("?pos={pos}&timeout={POLL_TIMEOUT}"));

	let url = format!(
		"{}/_matrix/client/unstable/org.matrix.simplified_msc3575/sync{query}",
		self.base
	);

	self.services
		.client
		.clients
		.default
		.post(url)
		.bearer_auth(self.token)
		.json(&body)
		.send()
		.await?
		.error_for_status()?
		.json()
		.await
		.map_err(Into::into)
}
