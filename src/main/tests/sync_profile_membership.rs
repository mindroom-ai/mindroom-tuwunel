#![cfg(test)]

use std::net::TcpListener;

use futures::future::join;
use serde_json::{Value, json};
use tuwunel::{Args, Runtime, Server, async_run, async_start, async_stop};
use tuwunel_core::{Result, implement, ruma::UserId};
use tuwunel_service::Services;

use self::client::{Client, field, register, wait_until_ready};

mod client;

const OWNER_TOKEN: &str = "sync-profile-membership-owner-token";
const DEVICE_TOKEN: &str = "sync-profile-membership-second-device-token";
const PEER_TOKEN: &str = "sync-profile-membership-peer-token";

/// Profile fanout changes timeline state, not when the account joined.
#[test]
fn profile_updates_preserve_incremental_sync_but_rejoins_reset_it() -> Result {
	let listener = TcpListener::bind(("127.0.0.1", 0))?;
	let port = listener.local_addr()?.port();
	let args = Args::default_test(&["fresh", "cleanup"])
		.with_option("address=[\"127.0.0.1\"]")
		.with_option(format!("port={port}"))
		.with_option("listening=true")
		.with_option("allow_local_presence=false")
		.with_option("allow_outgoing_presence=false");
	let runtime = Runtime::new(Some(&args))?;
	let server = Server::new(Some(&args), Some(&runtime))?;

	runtime.block_on(async {
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
	})
}

async fn exercise(services: &Services, base: &str) -> Result {
	wait_until_ready(services, base).await?;

	let owner_id = register(services, "profileowner", OWNER_TOKEN).await?;
	let peer_id = register(services, "profilepeer", PEER_TOKEN).await?;

	services
		.users
		.create_device(&owner_id, None, (Some(DEVICE_TOKEN), None), None, None, None)
		.await?;

	let owner = Client { services, base, token: OWNER_TOKEN };
	let device = Client { services, base, token: DEVICE_TOKEN };
	let peer = Client { services, base, token: PEER_TOKEN };
	let mut rooms = Vec::new();

	for _ in 0..2 {
		let room = peer
			.create_room(&json!({ "preset": "public_chat" }))
			.await?;

		owner
			.post(&format!("rooms/{room}/join"), &json!({}))
			.await?;
		rooms.push(room);
	}

	let mut opening = owner.sync(None).await?;
	let mut second = device.sync(None).await?;
	let mut sliding = owner.sliding(None).await?;
	let joined = services
		.state_cache
		.get_joined_count(&rooms[0], &owner_id)
		.await?;

	for (name, value) in [("displayname", "renamed"), ("avatar_url", "mxc://example.org/avatar")]
	{
		owner
			.put_json(&format!("profile/{owner_id}/{name}"), &json!({ name: value }))
			.await?;

		opening = owner
			.sync(Some(field(&opening, "next_batch")?))
			.await?;
		second = device
			.sync(Some(field(&second, "next_batch")?))
			.await?;
		sliding = owner
			.sliding(Some(field(&sliding, "pos")?))
			.await?;

		for room in &rooms {
			for response in [&opening, &second] {
				let timeline = &response["rooms"]["join"][room.as_str()]["timeline"];

				assert_ne!(timeline["limited"], true, "profile update reset {room}");
				assert_member(&timeline["events"], &owner_id, name, value);
			}

			let update = &sliding["rooms"][room.as_str()];

			assert_ne!(update["limited"], true, "sliding profile update reset {room}");
			assert_member(&update["timeline"], &owner_id, name, value);
		}

		let changed = &sliding["extensions"]["e2ee"]["device_lists"]["changed"];

		assert!(
			!changed
				.as_array()
				.is_some_and(|users| users.iter().any(|id| id == peer_id.as_str())),
			"profile update burst the room's device lists"
		);
		assert_eq!(
			services
				.state_cache
				.get_joined_count(&rooms[0], &owner_id)
				.await?,
			joined,
			"profile update moved the join position"
		);
	}

	// Leaving removes the join row, so a later join must get a fresh position.
	let room = &rooms[0];

	owner
		.post(&format!("rooms/{room}/leave"), &json!({}))
		.await?;
	owner
		.post(&format!("rooms/{room}/join"), &json!({}))
		.await?;

	for (client, previous) in [(&owner, &opening), (&device, &second)] {
		let response = client
			.sync(Some(field(previous, "next_batch")?))
			.await?;

		assert_eq!(
			response["rooms"]["join"][room.as_str()]["timeline"]["limited"],
			true,
			"real join did not reset the timeline"
		);
	}

	assert!(
		services
			.state_cache
			.get_joined_count(room, &owner_id)
			.await?
			> joined,
		"real join did not advance the join position"
	);

	let rejoined = owner
		.sliding(Some(field(&sliding, "pos")?))
		.await?;
	let changed = rejoined["extensions"]["e2ee"]["device_lists"]["changed"]
		.as_array()
		.expect("rejoin device list");

	assert!(
		changed.iter().any(|id| id == peer_id.as_str()),
		"real join omitted peer device list"
	);

	Ok(())
}

fn assert_member(events: &Value, user_id: &UserId, name: &str, value: &str) {
	let events = events.as_array().expect("room timeline events");

	assert!(
		events.iter().any(|event| {
			event["type"] == "m.room.member"
				&& event["state_key"] == user_id.as_str()
				&& event["content"]["membership"] == "join"
				&& event["content"][name] == value
		}),
		"timeline omitted the {name} member update: {events:?}"
	);
}

#[implement(Client, params = "<'_>")]
async fn put_json(&self, path: &str, body: &Value) -> Result {
	self.services
		.client
		.clients
		.default
		.put(self.url(path))
		.bearer_auth(self.token)
		.json(body)
		.send()
		.await?
		.error_for_status()?;

	Ok(())
}

#[implement(Client, params = "<'_>")]
async fn sync(&self, since: Option<&str>) -> Result<Value> {
	let query = since
		.map(|since| ("since", since))
		.into_iter()
		.collect::<Vec<_>>();

	self.services
		.client
		.clients
		.default
		.get(self.url("sync?timeout=0"))
		.bearer_auth(self.token)
		.query(&query)
		.send()
		.await?
		.error_for_status()?
		.json()
		.await
		.map_err(Into::into)
}

#[implement(Client, params = "<'_>")]
async fn sliding(&self, pos: Option<&str>) -> Result<Value> {
	let query = pos
		.map(|pos| ("pos", pos))
		.into_iter()
		.collect::<Vec<_>>();
	let url = format!("{}/_matrix/client/unstable/org.matrix.simplified_msc3575/sync", self.base);
	let body = json!({
		"lists": { "all": { "ranges": [[0, 9]], "timeline_limit": 10 } },
		"extensions": { "e2ee": { "enabled": true } },
	});

	self.services
		.client
		.clients
		.default
		.post(url)
		.bearer_auth(self.token)
		.query(&query)
		.json(&body)
		.send()
		.await?
		.error_for_status()?
		.json()
		.await
		.map_err(Into::into)
}
