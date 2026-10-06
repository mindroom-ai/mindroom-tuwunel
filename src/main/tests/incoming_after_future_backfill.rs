#![cfg(test)]

use serde_json::{json, value::RawValue as RawJsonValue};
use tuwunel_core::{
	Result,
	pdu::PduBuilder,
	ruma::{
		MilliSecondsSinceUnixEpoch, OwnedEventId, RoomId, UInt, UserId,
		events::room::message::RoomMessageEventContent,
	},
};
use tuwunel_service::Services;

use self::{
	client::{Client, register},
	fixture::boot,
};

mod client;
mod fixture;

const TOKEN: &str = "incoming-after-future-backfill-access-token";

/// A backfilled event dated in the future does not make later live events
/// look old.
///
/// Backfilled events sort before the rest of the timeline but carry whatever
/// timestamp their server set, so the old-event cutoff must not come from one.
#[test]
fn incoming_after_future_backfill_is_accepted() -> Result {
	let options: [&str; 0] = [];

	boot("incoming-after-future-backfill", options, exercise)
}

async fn exercise(services: &Services, base: &str) -> Result {
	let user_id = register(services, "backfillcutoff", TOKEN).await?;
	let client = Client { services, base, token: TOKEN };
	let room_id = client
		.create_room(&json!({ "preset": "private_chat" }))
		.await?;

	// A timestamp far in the future, as a remote server may set on an event.
	let future_ts = MilliSecondsSinceUnixEpoch(UInt::new_saturating(8_000_000_000_000));
	let (future_id, future) =
		make_pdu(services, &user_id, &room_id, Some(future_ts), "future").await?;
	let server_name = services.globals.server_name();

	services
		.timeline
		.backfill_pdu(&room_id, server_name, future)
		.await?;

	let first = services
		.timeline
		.first_pdu_in_room(&room_id)
		.await?;

	assert_eq!(first.event_id, future_id, "the backfilled event is not the room's first");

	let (_, live) = make_pdu(services, &user_id, &room_id, None, "live").await?;
	let (_, event_id, live) = services
		.event_handler
		.parse_incoming_pdu(&live)
		.await?;

	let handled = services
		.event_handler
		.handle_incoming_pdu(server_name, &room_id, &event_id, live, true)
		.await?;

	assert!(handled.is_some(), "the live event was skipped as old");

	services.timeline.get_pdu_id(&event_id).await?;

	Ok(())
}

/// Sign a message from a local user without appending it, formatted as a
/// remote server would send it.
async fn make_pdu(
	services: &Services,
	user_id: &UserId,
	room_id: &RoomId,
	timestamp: Option<MilliSecondsSinceUnixEpoch>,
	body: &str,
) -> Result<(OwnedEventId, Box<RawJsonValue>)> {
	let builder = PduBuilder {
		timestamp,
		..PduBuilder::timeline(&RoomMessageEventContent::text_plain(body))
	};

	let state_lock = services.state.mutex.lock(room_id).await;
	let (event, event_json) = services
		.timeline
		.create_hash_and_sign_event(builder, user_id, room_id, &state_lock)
		.await?;

	drop(state_lock);

	let pdu = services
		.federation
		.format_pdu_into(event_json, None)
		.await;

	Ok((event.event_id, pdu))
}
