use std::collections::{HashMap, HashSet};

use axum::extract::State;
use futures::{StreamExt, future::join3, pin_mut};
use ruma::{
	MilliSecondsSinceUnixEpoch, OwnedEventId, UserId,
	api::client::push::get_notifications::{self, v3::Notification},
	push::Action,
};
use tuwunel_core::{
	Result, at, err,
	result::LogErr,
	utils::{
		stream::{ReadyExt, WidebandExt},
		string::to_small_string,
	},
};
use tuwunel_matrix::{Event, PduCount, PduId, ShortRoomId};
use tuwunel_service::Services;

use crate::Ruma;

/// A page entry with its room and the root of its thread, if it is in one.
type Entry = (u64, Notification, (ShortRoomId, Option<OwnedEventId>));

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
				.filter(|event| !event.is_redacted())?
				.without_transaction_id_unless_sender(Some(sender_user))
				.log_err()
				.ok()?;

			// Entries are read by `read_entries` below.
			let thread = services.threads.get_thread_id(&event).await;

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
				read: false,
				profile_tag: notify.tag,
				actions: notify.actions,
			};

			Some((count, notification, (pdu_id.shortroomid, thread)))
		})
		// One notification past the page shows whether another page follows.
		.take(limit.saturating_add(1))
		.collect::<Vec<_>>()
		.await;

	let more = notifications.len() > limit;
	notifications.truncate(limit);
	read_entries(&services, sender_user, &mut notifications).await;

	Ok(get_notifications::v3::Response {
		next_token: notifications
			.last()
			.filter(|_| more)
			.map(at!(0))
			.map(to_small_string),

		notifications: notifications.into_iter().map(at!(1)).collect(),
	})
}

/// Marks each entry read once `N` of the user's notifying events and `H` of
/// their highlighting events in its thread, or in its room's main timeline, are
/// newer, `N` and `H` being that timeline's sync counts, which hold the user's
/// newest such events there. One walk down the user's notifications finds them
/// for the whole page, and stops once every timeline has.
async fn read_entries(services: &Services, user: &UserId, page: &mut [Entry]) {
	// Per timeline, for its notifying and its highlighting events: how many are
	// left to find, and the last one found.
	let (mut timelines, mut rooms, mut oldest) = (HashMap::new(), HashSet::new(), u64::MAX);
	let mut counts = HashMap::new();
	for (count, notification, timeline) in page.iter() {
		let room = &notification.room_id;
		if !counts.contains_key(room) {
			let (notifications, highlights, threads) = join3(
				services.pusher.notification_count(user, room),
				services.pusher.highlight_count(user, room),
				services
					.pusher
					.thread_notification_counts(user, room),
			)
			.await;

			counts.insert(room.clone(), ((notifications, highlights), threads));
		}

		let (main, threads) = &counts[room];
		let unread = match &timeline.1 {
			| None => *main,
			| Some(root) => threads.get(root).copied().unwrap_or_default(),
		};

		timelines.insert(timeline.clone(), [(unread.0, u64::MAX), (unread.1, u64::MAX)]);
		if unread != (0, 0) {
			rooms.insert(timeline.0);
			oldest = oldest.min(*count);
		}
	}

	let rows = services
		.pusher
		.get_notifications(user, None)
		.ready_take_while(|(count, _)| *count >= oldest)
		.ready_filter(|(_, notified)| rooms.contains(&notified.sroomid));

	pin_mut!(rows);
	let mut pending = timelines
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

		// Recounts skip redacted events, and a redacted reply names no thread, so
		// it would count in its room's main timeline.
		let Ok(pdu) = services
			.timeline
			.get_pdu_from_id(&pdu_id.into())
			.await
		else {
			continue;
		};
		if pdu.is_redacted() {
			continue;
		}

		let counted = [
			notified.actions.iter().any(Action::should_notify),
			notified.actions.iter().any(Action::is_highlight),
		];
		let timeline = (notified.sroomid, services.threads.get_thread_id(&pdu).await);
		let found = timelines.get_mut(&timeline);
		for ((left, last), counted) in found.into_iter().flatten().zip(counted) {
			if counted && *left > 0 {
				*left = left.saturating_sub(1);
				*last = count;
				pending = pending.saturating_sub(usize::from(*left == 0));
			}
		}
	}

	for (count, notification, timeline) in page.iter_mut() {
		notification.read = timelines[&*timeline]
			.iter()
			.all(|(left, last)| *left == 0 && *count < *last);
	}
}
