//! What an external account learns of rooms it is not in: the room
//! directory, room summaries and space hierarchies show it only rooms it is
//! joined or invited to.

use std::slice;

use reqwest::{Response, StatusCode};
use serde_json::{Value, json};
use tuwunel_core::{
	Result,
	ruma::{OwnedRoomId, RoomId},
};

use super::{
	AMY_TOKEN, Client, GUEST_TOKEN, creator, forbidden, make_rooms, membership::room_path, run,
	run_with, sign_in_with,
};

/// The refusal of an external account's look at a room it is not in.
const UNSEEN: &str = "This account may see a room only when it is in it or invited to it.";

/// The access token of an appservice.
const BRIDGE_TOKEN: &str = "room-visibility-appservice-token-0123456789";

/// The room directory lists an external account only the rooms it is joined
/// or invited to, searched or not, and counts no other; another server's
/// directory is empty to it, unasked; a staff account still lists every
/// published room.
#[test]
fn external_account_lists_only_its_rooms() -> Result {
	run("sso-room-visibility-directory", async |services, base| {
		let rooms = make_rooms(services, base).await?;
		let creator = creator(services, base);
		let published = [&rooms.public, &rooms.open, &rooms.class, &rooms.staff];

		for room_id in published {
			creator
				.put(
					&format!("directory/list/room/{room_id}"),
					&json!({ "visibility": "public" }),
				)
				.await?;
		}

		let (guest_id, guest) = sign_in_with(services, base, "guest", GUEST_TOKEN).await?;
		let (_, amy) = sign_in_with(services, base, "amy", AMY_TOKEN).await?;

		let listing = guest.get("publicRooms").await?;

		assert_eq!(room_ids(&listing), slice::from_ref(&rooms.class));
		assert_eq!(listing["total_room_count_estimate"], 1);
		assert_eq!(searched(&guest, "open").await?, Vec::<OwnedRoomId>::new());
		assert_eq!(searched(&guest, "class").await?, slice::from_ref(&rooms.class));

		creator
			.act_on(&rooms.open, "invite", &guest_id)
			.await?;

		assert_eq!(searched(&guest, "open").await?, slice::from_ref(&rooms.open));
		assert_eq!(searched(&guest, "public").await?, Vec::<OwnedRoomId>::new());

		let remote = guest
			.get("publicRooms?server=remote.example")
			.await?;

		assert_eq!(room_ids(&remote), Vec::<OwnedRoomId>::new());
		assert_eq!(remote["total_room_count_estimate"], 0);

		assert_eq!(
			room_ids(&amy.get("publicRooms").await?),
			sorted(published.into_iter().cloned())
		);
		assert_eq!(searched(&amy, "public").await?, slice::from_ref(&rooms.public));

		Ok(())
	})
}

/// An external account gets no summary of a room it is neither in nor
/// invited to, by ID or by alias, although the room is public and a staff
/// account gets it; its own room and a room that invited it are summarized.
/// Without an access token there is no summary at all, so dropping the token
/// is no way around.
#[test]
fn external_account_sees_no_summary_of_other_rooms() -> Result {
	run("sso-room-visibility-summary", async |services, base| {
		let rooms = make_rooms(services, base).await?;
		let creator = creator(services, base);
		let (guest_id, guest) = sign_in_with(services, base, "guest", GUEST_TOKEN).await?;
		let (_, amy) = sign_in_with(services, base, "amy", AMY_TOKEN).await?;

		forbidden(summary(&guest, rooms.public.as_str()).await?, UNSEEN).await?;
		forbidden(summary(&guest, "%23public:localhost").await?, UNSEEN).await?;

		let unsigned = unsigned_summary(&guest, &rooms.public).await?;

		assert_eq!(unsigned.status(), StatusCode::UNAUTHORIZED);
		assert_eq!(unsigned.json::<Value>().await?["errcode"], "M_MISSING_TOKEN");

		// An appservice's token names no user, and is no missing token.
		let registration = json!({
			"id": "bridge",
			"url": null,
			"as_token": BRIDGE_TOKEN,
			"hs_token": "room-visibility-homeserver-token-0123456789",
			"sender_localpart": "bridge",
			"namespaces": {"users": [], "aliases": [], "rooms": []}
		});

		services
			.appservice
			.register_appservice(serde_json::from_value(registration)?)
			.await?;

		let bridge = Client { services, base, token: BRIDGE_TOKEN };

		assert_eq!(
			summary(&bridge, rooms.public.as_str())
				.await?
				.status(),
			StatusCode::OK
		);

		assert_eq!(
			summary(&guest, rooms.class.as_str())
				.await?
				.status(),
			StatusCode::OK
		);
		assert_eq!(
			summary(&amy, rooms.public.as_str())
				.await?
				.status(),
			StatusCode::OK
		);

		creator
			.act_on(&rooms.public, "invite", &guest_id)
			.await?;

		assert_eq!(
			summary(&guest, rooms.public.as_str())
				.await?
				.status(),
			StatusCode::OK
		);

		Ok(())
	})
}

/// Where the room directory is open without an access token, so is the summary
/// of a public room, and of no private one.
#[test]
fn summary_without_a_token_follows_the_directory() -> Result {
	let open = ["directory.rooms.allow_without_auth=true"];

	run_with("sso-room-visibility-unsigned-summary", &open, async |services, base| {
		let rooms = make_rooms(services, base).await?;
		let creator = creator(services, base);

		assert_eq!(
			unsigned_summary(&creator, &rooms.public)
				.await?
				.status(),
			StatusCode::OK
		);
		assert_eq!(
			unsigned_summary(&creator, &rooms.class)
				.await?
				.status(),
			StatusCode::FORBIDDEN
		);

		Ok(())
	})
}

/// An external account walks no space it is neither in nor invited to, here
/// or on another server, and in a space that invited it finds only the
/// children it is in, although the space and another child are public; a
/// staff account walks the space and finds its public child.
#[test]
fn external_account_walks_only_its_rooms_in_a_space() -> Result {
	run("sso-room-visibility-hierarchy", async |services, base| {
		let rooms = make_rooms(services, base).await?;
		let creator = creator(services, base);
		let space = creator
			.create_room(&json!({
				"preset": "public_chat",
				"creation_content": { "type": "m.space" },
			}))
			.await?;

		for child in [&rooms.public, &rooms.class] {
			creator
				.put(
					&room_path(&space, &format!("state/m.space.child/{child}")),
					&json!({ "via": ["localhost"] }),
				)
				.await?;
		}

		let (guest_id, guest) = sign_in_with(services, base, "guest", GUEST_TOKEN).await?;
		let (_, amy) = sign_in_with(services, base, "amy", AMY_TOKEN).await?;

		forbidden(hierarchy(&guest, &space).await?, "The requested room is inaccessible.")
			.await?;

		// No other server is asked about a room the account is not in.
		let remote = RoomId::parse("!space:remote.example")?;

		forbidden(hierarchy(&guest, &remote).await?, "The requested room is inaccessible.")
			.await?;

		let walked = async |client: &Client<'_>| -> Result<Vec<OwnedRoomId>> {
			let response = hierarchy(client, &space)
				.await?
				.error_for_status()?;

			Ok(room_ids_of(&response.json::<Value>().await?["rooms"]))
		};

		assert_eq!(walked(&amy).await?, sorted([space.clone(), rooms.public.clone()]));

		creator
			.act_on(&space, "invite", &guest_id)
			.await?;

		assert_eq!(walked(&guest).await?, sorted([space.clone(), rooms.class.clone()]));

		Ok(())
	})
}

/// The rooms a directory search for `term` lists the client, sorted.
async fn searched(client: &Client<'_>, term: &str) -> Result<Vec<OwnedRoomId>> {
	let listing = client
		.post("publicRooms", &json!({ "filter": { "generic_search_term": term } }))
		.await?;

	Ok(room_ids(&listing))
}

/// The rooms of a directory listing, sorted.
fn room_ids(listing: &Value) -> Vec<OwnedRoomId> { room_ids_of(&listing["chunk"]) }

/// The room IDs of an array of rooms, sorted.
fn room_ids_of(rooms: &Value) -> Vec<OwnedRoomId> {
	sorted(
		rooms
			.as_array()
			.into_iter()
			.flatten()
			.filter_map(|room| room["room_id"].as_str())
			.filter_map(|room_id| room_id.try_into().ok()),
	)
}

fn sorted(rooms: impl IntoIterator<Item = OwnedRoomId>) -> Vec<OwnedRoomId> {
	let mut rooms: Vec<_> = rooms.into_iter().collect();
	rooms.sort();

	rooms
}

/// Ask for the summary of a room, by ID or by percent-encoded alias.
async fn summary(client: &Client<'_>, room: &str) -> Result<Response> {
	client
		.get_url(&client.client_url("v1/", &format!("room_summary/{room}")))
		.await
}

/// Ask for the summary of a room without an access token, on the client's
/// server.
async fn unsigned_summary(client: &Client<'_>, room: &RoomId) -> Result<Response> {
	let response = client
		.services
		.client
		.clients
		.default
		.get(client.client_url("v1/", &format!("room_summary/{room}")))
		.send()
		.await?;

	Ok(response)
}

/// Ask for the hierarchy of a space.
async fn hierarchy(client: &Client<'_>, space: &RoomId) -> Result<Response> {
	client
		.get_url(&client.client_url("v1/", &room_path(space, "hierarchy")))
		.await
}
