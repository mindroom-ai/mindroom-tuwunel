use axum::extract::State;
use futures::StreamExt;
use ruma::{
	EventId, MilliSecondsSinceUnixEpoch, RoomId, UserId, api::client::push::get_notifications,
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
					thread_read(&services, sender_user, event.room_id(), &root, pdu_id).await,
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

/// Whether the entry at `pdu_id` in the thread of `root` is read. The thread's
/// sync count is the user's newest notifying events in the thread, so the entry
/// is read once that many of them follow it.
async fn thread_read(
	services: &Services,
	user: &UserId,
	room: &RoomId,
	root: &EventId,
	pdu_id: PduId,
) -> bool {
	let unread = services
		.pusher
		.thread_notification_counts(user, room)
		.await
		.get(root)
		.map_or(0, at!(0));

	let after = services
		.pusher
		.get_notifications(user, None)
		.ready_take_while(|(later, _)| PduCount::Normal(*later) > pdu_id.count)
		.ready_filter(|(_, notified)| {
			notified.sroomid == pdu_id.shortroomid
				&& notified.actions.iter().any(Action::should_notify)
		})
		.filter_map(async |(later, _)| {
			let later = PduId { count: PduCount::Normal(later), ..pdu_id };
			let pdu = services
				.timeline
				.get_pdu_from_id(&later.into())
				.await
				.ok()?;
			let thread = services.threads.get_thread_id(&pdu).await;

			(thread.as_deref() == Some(root)).then_some(())
		})
		.take(usize::try_from(unread).unwrap_or(usize::MAX))
		.count()
		.await;

	u64::try_from(after).is_ok_and(|after| after >= unread)
}
