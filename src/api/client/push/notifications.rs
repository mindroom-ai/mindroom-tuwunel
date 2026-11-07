use std::collections::BTreeMap;

use axum::extract::State;
use futures::StreamExt;
use ruma::{
	EventId, MilliSecondsSinceUnixEpoch, RoomId, UserId,
	api::{Direction::Forward, client::push::get_notifications},
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

/// Whether the reply at `count` in the thread of `root` is read: by the user's
/// unthreaded or thread read, public or private, at or after it, or by an own
/// reply in the thread after it. Thread read stamps are not consulted: an
/// unthreaded receipt on an older event deletes them.
async fn thread_read(
	services: &Services,
	user: &UserId,
	room: &RoomId,
	root: &EventId,
	count: u64,
) -> bool {
	let receipts = &services.read_receipt;
	for thread in [ReceiptThread::Unthreaded, ReceiptThread::Thread(root.to_owned())] {
		let read = receipts
			.private_read_position(room, user, &thread)
			.await;

		if read.is_ok_and(|(read, _)| read >= count) {
			return true;
		}
	}

	// A receipt is stored after its event, so only rows past `count` can cover it.
	let public = receipts
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
		});

	if public.await {
		return true;
	}

	// An unthreaded private receipt drops the private thread marker an own
	// reply left, so the reply itself is looked up.
	let Ok(root_id) = services.timeline.get_pdu_id(root).await else {
		return false;
	};
	let PduId { shortroomid, count: root_count } = root_id.into();

	services
		.pdu_metadata
		.get_relations(shortroomid, root_count, Some(PduCount::Normal(count)), Forward, None)
		.ready_filter(|(_, pdu)| pdu.sender() == user)
		.any(async |(_, pdu)| {
			services
				.threads
				.get_thread_id(&pdu)
				.await
				.as_deref()
				== Some(root)
		})
		.await
}
