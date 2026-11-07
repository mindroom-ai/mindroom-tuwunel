use std::collections::BTreeMap;

use axum::extract::State;
use futures::StreamExt;
use ruma::{
	EventId, MilliSecondsSinceUnixEpoch, RoomId, UserId,
	api::client::push::get_notifications,
	events::{AnySyncEphemeralRoomEvent, receipt::ReceiptThread},
	push::Action,
};
use tuwunel_core::{
	Result, at, err,
	matrix::{Event, PduCount, PduId},
	utils::{
		stream::{ReadyExt, WidebandExt},
		string::to_small_string,
	},
};
use tuwunel_service::Services;

use crate::Ruma;

/// # `GET /_matrix/client/r0/notifications/`
///
/// Paginate through the list of events the user has been, or would have been
/// notified about.
pub(crate) async fn get_notifications_route(
	State(services): State<crate::State>,
	body: Ruma<get_notifications::v3::Request>,
) -> Result<get_notifications::v3::Response> {
	use get_notifications::v3::Notification;

	let sender_user = body.sender_user();

	let from = body
		.body
		.from
		.as_deref()
		.map(str::parse)
		.transpose()
		.map_err(|e| err!(Request(InvalidParam("Invalid `from' parameter: {e}"))))?;

	let limit: usize = body
		.body
		.limit
		.map(TryInto::try_into)
		.transpose()?
		.unwrap_or(50)
		.clamp(1, 100);

	let only_highlight = body
		.body
		.only
		.as_deref()
		.is_some_and(|only| only.contains("highlight"));

	let mut notifications = services
		.pusher
		.get_notifications(sender_user, from)
		.ready_filter(|(_, notify)| {
			!only_highlight || notify.actions.iter().any(Action::is_highlight)
		})
		.wide_filter_map(async |(count, notify)| {
			let pdu_id = PduId {
				shortroomid: notify.sroomid,
				count: count.into(),
			};

			let event = services
				.timeline
				.get_pdu_from_id(&pdu_id.into())
				.await
				.ok()
				.filter(|event| !event.is_redacted())?;

			let read = match services.threads.get_thread_id(&event).await {
				| None => services
					.pusher
					.last_notification_read(sender_user, event.room_id())
					.await
					.is_ok_and(|last_read| last_read.ge(&count)),
				| Some(root) =>
					thread_read(&services, sender_user, event.room_id(), &root, count).await,
			};

			let ts = notify
				.ts
				.try_into()
				.map(MilliSecondsSinceUnixEpoch)
				.ok()?;

			let event = services
				.pdu_metadata
				.bundle_aggregations(sender_user, event)
				.await;

			let notification = Notification {
				room_id: event.room_id().into(),
				event: event.into_format(),
				ts,
				read,
				profile_tag: notify.tag,
				actions: notify.actions,
			};

			Some((count, notification))
		})
		// One notification past the page shows whether another page follows.
		.take(limit.saturating_add(1))
		.collect::<Vec<_>>()
		.await;

	let more = notifications.len() > limit;
	notifications.truncate(limit);

	Ok(get_notifications::v3::Response {
		next_token: notifications
			.last()
			.filter(|_| more)
			.map(at!(0))
			.map(to_small_string),

		notifications: notifications.into_iter().map(at!(1)).collect(),
	})
}

/// Whether the reply at `count` in the thread of `root` is read. A `"main"` read
/// also advances the room watermark, so past the thread's own watermark only
/// the user's unthreaded receipt, or a receipt in the thread, reads the reply.
async fn thread_read(
	services: &Services,
	user: &UserId,
	room: &RoomId,
	root: &EventId,
	count: u64,
) -> bool {
	let covers = |last_read: Result<u64>| last_read.is_ok_and(|last_read| last_read >= count);

	if covers(
		services
			.pusher
			.last_thread_notification_read(user, room, root)
			.await,
	) {
		return true;
	}

	// Every read through `count` advanced the room watermark past it.
	if !covers(
		services
			.pusher
			.last_notification_read(user, room)
			.await,
	) {
		return false;
	}

	let private = services
		.read_receipt
		.private_read_get_count(room, user)
		.await;

	if covers(private.map(|(read, _)| read)) {
		return true;
	}

	// A receipt is stored after its event, so only rows past `count` can cover it.
	services
		.read_receipt
		.readreceipts_since(room, count, None)
		.ready_filter(|(receipt_user, ..)| *receipt_user == user)
		.ready_filter_map(|(_, _, raw)| {
			let AnySyncEphemeralRoomEvent::Receipt(receipt) = raw.deserialize().ok()? else {
				return None;
			};
			let (event_id, receipts) = receipt.content.0.into_iter().next()?;
			let receipt = receipts
				.into_values()
				.flat_map(BTreeMap::into_values)
				.next()?;

			match receipt.thread {
				| ReceiptThread::Unthreaded => Some(event_id),
				| ReceiptThread::Thread(thread) if *thread == *root => Some(event_id),
				| _ => None,
			}
		})
		.any(async |event_id| {
			services
				.timeline
				.get_pdu_count(&event_id)
				.await
				.is_ok_and(|read| read >= PduCount::Normal(count))
		})
		.await
}
