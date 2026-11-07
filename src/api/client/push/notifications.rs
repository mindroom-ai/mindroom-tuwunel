use std::collections::{HashMap, HashSet};

use axum::extract::State;
use futures::{StreamExt, pin_mut};
use ruma::{
	MilliSecondsSinceUnixEpoch, OwnedEventId, UserId,
	api::client::push::get_notifications::{self, v3::Notification},
	push::Action,
};
use tuwunel_core::{
	Result, at, err,
	matrix::{Event, PduCount, PduId, ShortRoomId},
	utils::{
		stream::{ReadyExt, WidebandExt},
		string::to_small_string,
	},
};
use tuwunel_service::Services;

use crate::Ruma;

/// A page entry with the room and root of its thread, if it is in one.
type Entry = (u64, Notification, Option<(ShortRoomId, OwnedEventId)>);

/// # `GET /_matrix/client/r0/notifications/`
///
/// Paginate through the list of events the user has been, or would have been
/// notified about.
pub(crate) async fn get_notifications_route(
	State(services): State<crate::State>,
	body: Ruma<get_notifications::v3::Request>,
) -> Result<get_notifications::v3::Response> {
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

			// Thread entries are read by `read_threads` below.
			let thread = services.threads.get_thread_id(&event).await;
			let read = thread.is_none()
				&& services
					.pusher
					.last_notification_read(sender_user, event.room_id())
					.await
					.is_ok_and(|last_read| last_read.ge(&count));

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

			Some((count, notification, thread.map(|root| (pdu_id.shortroomid, root))))
		})
		// One notification past the page shows whether another page follows.
		.take(limit.saturating_add(1))
		.collect::<Vec<_>>()
		.await;

	let more = notifications.len() > limit;
	notifications.truncate(limit);
	read_threads(&services, sender_user, &mut notifications).await;

	Ok(get_notifications::v3::Response {
		next_token: notifications
			.last()
			.filter(|_| more)
			.map(at!(0))
			.map(to_small_string),

		notifications: notifications.into_iter().map(at!(1)).collect(),
	})
}

/// Marks each thread entry read once `N` of the user's notifying events and `H`
/// of their highlighting events in its thread are newer, `N` and `H` being the
/// thread's sync counts, which hold the user's newest such events in the
/// thread. One walk down the user's notifications finds them for the whole
/// page, and stops once every thread has.
async fn read_threads(services: &Services, user: &UserId, page: &mut [Entry]) {
	// Per thread, for its notifying and its highlighting events: how many are
	// left to find, and the last one found.
	let (mut threads, mut rooms, mut oldest) = (HashMap::new(), HashSet::new(), u64::MAX);
	let mut counts = HashMap::new();
	for (count, notification, thread) in page.iter() {
		let Some((room, root)) = thread else {
			continue;
		};

		if !counts.contains_key(&notification.room_id) {
			let room_counts = services
				.pusher
				.thread_notification_counts(user, &notification.room_id)
				.await;

			counts.insert(notification.room_id.clone(), room_counts);
		}

		let unread = counts[&notification.room_id]
			.get(root)
			.copied()
			.unwrap_or_default();

		threads.insert(root.clone(), [(unread.0, u64::MAX), (unread.1, u64::MAX)]);
		if unread != (0, 0) {
			rooms.insert(*room);
			oldest = oldest.min(*count);
		}
	}

	let rows = services
		.pusher
		.get_notifications(user, None)
		.ready_take_while(|(count, _)| *count >= oldest)
		.ready_filter(|(_, notified)| rooms.contains(&notified.sroomid));

	pin_mut!(rows);
	let mut pending = threads
		.values()
		.flatten()
		.filter(|(left, _)| *left > 0)
		.count();
	while pending > 0
		&& let Some((count, notified)) = rows.next().await
	{
		let pdu_id = PduId {
			shortroomid: notified.sroomid,
			count: PduCount::Normal(count),
		};

		let Ok(pdu) = services
			.timeline
			.get_pdu_from_id(&pdu_id.into())
			.await
		else {
			continue;
		};

		let counted = [
			notified.actions.iter().any(Action::should_notify),
			notified.actions.iter().any(Action::is_highlight),
		];
		let thread = services.threads.get_thread_id(&pdu).await;
		let found = thread.and_then(|root| threads.get_mut(&root));
		for ((left, last), counted) in found.into_iter().flatten().zip(counted) {
			if counted && *left > 0 {
				*left = left.saturating_sub(1);
				*last = count;
				pending = pending.saturating_sub(usize::from(*left == 0));
			}
		}
	}

	for (count, notification, thread) in page.iter_mut() {
		if let Some((_, root)) = thread {
			notification.read = threads[root]
				.iter()
				.all(|(left, last)| *left == 0 && *count < *last);
		}
	}
}
