use std::{
	collections::{BTreeMap, BTreeSet},
	pin::pin,
	sync::Arc,
};

use futures::{StreamExt, future::join3, stream::select};
use ruma::{
	EventId, OwnedEventId, OwnedRoomId, OwnedUserId, RoomId, UserId,
	events::receipt::ReceiptThread,
};
use tuwunel_core::{
	Result, implement,
	matrix::{event::Event, pdu::PduCount},
	trace,
	utils::{
		result::LogErr,
		stream::{BroadbandExt, ReadyExt, TryIgnore},
		u64_from_u8,
	},
};
use tuwunel_database::{
	Deserialized, Ignore, IgnoreAll, Interfix, KeyBuf, Map,
	deserialize_from_slice as deserialize_key, serialize_key,
};

/// Per-thread unread counts: `(notification, highlight)` keyed by thread root.
type ThreadCounts = BTreeMap<OwnedEventId, (u64, u64)>;

/// Per-thread last-read counts keyed by thread root. Sync v3 uses them to
/// decide whether a round sends the room counts, and whether it may send them
/// as zero.
type ThreadLastReads = BTreeMap<OwnedEventId, u64>;

/// Bound every room scan, including sibling threads and events without push actions.
const RECOUNT_LIMIT: usize = 4096;
type Unread = BTreeMap<Option<OwnedEventId>, (u64, u64)>;

/// Read only through the acknowledged event. Own sends (`None`) and a recount
/// past its limit read through the present; a raced context keeps its counts.
#[implement(super::Service)]
pub async fn reset_notification_counts_for_thread(
	&self,
	user_id: &UserId,
	room_id: &RoomId,
	acknowledged: Option<&EventId>,
	thread: &ReceiptThread,
) {
	if matches!(thread, ReceiptThread::Thread(root) if acknowledged == Some(root)) {
		return;
	}
	let read_to = match acknowledged {
		| None => None,
		| Some(event) => match self.services.timeline.get_pdu_count(event).await {
			| Ok(PduCount::Normal(count)) => Some(count),
			| _ => return,
		},
	};
	let contexts = match thread {
		| ReceiptThread::Main => vec![None],
		| ReceiptThread::Thread(root) => vec![Some(root.clone())],
		| _ => std::iter::once(None)
			.chain(
				self.thread_notification_counts(user_id, room_id)
					.await
					.into_keys()
					.map(Some),
			)
			.collect(),
	};
	// These snapshots and the bounded PDU scan never hold the insertion lock.
	// Own sends read through the present, so they need none. The room stamp is
	// read first: a reset publishes it after its counts.
	let room_read = self
		.last_notification_read(user_id, room_id)
		.await
		.ok();
	let mut before = BTreeMap::new();
	for root in contexts {
		let snapshot = match read_to {
			| Some(_) => Some(
				self.notification_state(user_id, room_id, root.as_deref())
					.await,
			),
			| None => None,
		};
		before.insert(root, snapshot);
	}
	// No scan when every context is already at zero: none can go lower.
	let pending = before
		.values()
		.flatten()
		.any(|state| state.0 > 0 || state.1 > 0);
	let unread = match read_to {
		| Some(count) if pending => self
			.unread_after(user_id, room_id, count, thread)
			.await
			.unwrap_or_default(),
		| _ => Unread::new(),
	};
	let _lock = self
		.notification_increment_mutex
		.lock(&(room_id.to_owned(), user_id.to_owned()))
		.await;
	// A room-wide reset since the snapshot invalidates every context.
	let raced = read_to.is_some()
		&& self
			.last_notification_read(user_id, room_id)
			.await
			.ok()
			!= room_read;
	for (root, snapshot) in before {
		let left = match snapshot {
			| None => (0, 0),
			| Some(snapshot) => {
				if raced
					|| self
						.notification_state(user_id, room_id, root.as_deref())
						.await
						!= snapshot
				{
					continue;
				}
				let after = unread.get(&root).copied().unwrap_or_default();
				(snapshot.0.min(after.0), snapshot.1.min(after.1))
			},
		};
		self.put_counts(user_id, room_id, root.as_deref(), left);
		// A stamp takes its sequence number only for its own write, after the
		// counts it covers, so a receipt's recount holds no sequence number:
		// syncs wait on it only while a send holding its own sequence number
		// waits for this lock. Own sends and joins recount while holding their
		// event's sequence number.
		if let ReceiptThread::Thread(root) = thread {
			self.db
				.roomuserid_lastnotificationread
				.put((room_id, user_id, root), *self.services.globals.next_count());
		}
	}
	// Publish the room stamp even when a concurrent insertion kept the main
	// count unchanged, so every room-wide reset invalidates snapshots. It is
	// newer than every thread stamp an unthreaded receipt removes.
	if !matches!(thread, ReceiptThread::Thread(_)) {
		self.db
			.roomuserid_lastnotificationread
			.put((room_id, user_id), *self.services.globals.next_count());
		if matches!(thread, ReceiptThread::Unthreaded) {
			self.db
				.roomuserid_lastnotificationread
				.del_prefix(&(room_id, user_id, Interfix))
				.await;
		}
		self.clear_suppressed_room(user_id, room_id);
	}
}

/// Counts detect concurrent insertions; the thread's read stamp prevents ABA
/// when a concurrent receipt lowers counts and a later event raises them again.
#[implement(super::Service)]
async fn notification_state(
	&self,
	user: &UserId,
	room: &RoomId,
	root: Option<&EventId>,
) -> (u64, u64, u64) {
	let key = notification_key(user, room, root);
	let count = async |map: &Arc<Map>| {
		map.get(&key)
			.await
			.deserialized::<u64>()
			.unwrap_or(0)
	};
	join3(
		count(&self.db.userroomid_notificationcount),
		count(&self.db.userroomid_highlightcount),
		async {
			match root {
				| Some(root) => self
					.last_thread_notification_read(user, room, root)
					.await
					.unwrap_or(0),
				| None => 0,
			}
		},
	)
	.await
}

fn notification_key(user: &UserId, room: &RoomId, root: Option<&EventId>) -> KeyBuf {
	match root {
		| Some(root) => serialize_key((user, room, root)),
		| None => serialize_key((user, room)),
	}
	.expect("notification key")
}

/// Stores one context's counts; a thread without any keeps no row.
#[implement(super::Service)]
fn put_counts(&self, user: &UserId, room: &RoomId, root: Option<&EventId>, counts: (u64, u64)) {
	let key = notification_key(user, room, root);
	if root.is_some() && counts == (0, 0) {
		self.db.userroomid_notificationcount.remove(&key);
		self.db.userroomid_highlightcount.remove(&key);
	} else {
		self.db
			.userroomid_notificationcount
			.raw_put(&key, counts.0);
		self.db
			.userroomid_highlightcount
			.raw_put(&key, counts.1);
	}
}

/// What a redaction can take from local users' counts in its target's timeline.
pub struct RedactionCounts {
	room_id: OwnedRoomId,
	root: Option<OwnedEventId>,
	users: Vec<Held>,
}

/// One user's counts and read stamp as read, what is left of the counts to
/// find, and the events found holding them: `(notification, highlight)` each.
struct Held {
	user_id: OwnedUserId,
	before: (u64, u64, u64),
	left: (u64, u64),
	events: Vec<(OwnedEventId, (bool, bool))>,
}

/// Before `target` is redacted, finds which of it and the later events hold
/// local users' counts in its timeline; only those can leave it, as the target,
/// its edits, or events whose thread runs through it. Counts hold a user's
/// newest notifying and highlighting events, as `/notifications` reads them, so
/// the scan runs newest first. It stops at the target or once every user's
/// counts are found, and past `RECOUNT_LIMIT` events gives up, leaving them.
#[implement(super::Service)]
pub async fn redaction_counts(
	&self,
	room_id: &RoomId,
	target: &EventId,
) -> Option<RedactionCounts> {
	let timeline = &self.services.timeline;
	let PduCount::Normal(oldest) = timeline.get_pdu_count(target).await.ok()? else {
		return None;
	};
	let target = timeline
		.get_pdu(target)
		.await
		.ok()
		.filter(|pdu| !pdu.is_redacted())?;
	let root = self.services.threads.get_thread_id(&target).await;

	let local: Vec<OwnedUserId> = self
		.services
		.state_cache
		.active_local_users_in_room(room_id)
		.map(ToOwned::to_owned)
		.collect()
		.await;
	let mut users = Vec::new();
	for user_id in local {
		let before = self
			.notification_state(&user_id, room_id, root.as_deref())
			.await;
		let left = (before.0, before.1);
		if left != (0, 0) {
			users.push(Held {
				user_id,
				before,
				left,
				events: Vec::new(),
			});
		}
	}

	let pdus = timeline
		.pdus_rev(None, room_id, None)
		.take(RECOUNT_LIMIT.saturating_add(1));
	let mut pdus = pin!(pdus);
	while users.iter().any(|user| user.left != (0, 0)) {
		let (count, pdu) = pdus.next().await?.ok()?;
		if count < PduCount::Normal(oldest) {
			break;
		}
		if pdu.is_redacted() || self.services.threads.get_thread_id(&pdu).await != root {
			continue;
		}
		for user in users
			.iter_mut()
			.filter(|user| user.left != (0, 0))
		{
			let key = (&user.user_id, count.into_unsigned());
			let row: Result<super::Notified> = self
				.db
				.useridcount_notification
				.qry(&key)
				.await
				.deserialized();
			let (notify, highlight) = match row {
				| Ok(row) => super::append::counted(&row.actions),
				| Err(error) if error.is_not_found() => continue,
				| Err(_) => return None,
			};
			let held = (notify && user.left.0 > 0, highlight && user.left.1 > 0);
			user.left.0 = user.left.0.saturating_sub(held.0.into());
			user.left.1 = user.left.1.saturating_sub(held.1.into());
			if held.0 || held.1 {
				user.events
					.push((pdu.event_id().to_owned(), held));
			}
		}
	}

	(!users.is_empty()).then(|| RedactionCounts { room_id: room_id.to_owned(), root, users })
}

/// After a redaction, takes from each user's counts the events found holding
/// them that left the timeline: those redacted, and those whose thread ran
/// through a redacted event, which now belong to the main timeline. Only a
/// receipt can change the counts meanwhile, as the room's events wait for the
/// redaction; a user whose counts it changed keeps them, as a raced receipt does.
#[implement(super::Service)]
pub async fn drop_redacted_counts(&self, counts: RedactionCounts) {
	let RedactionCounts { room_id, root, users } = counts;
	let held: BTreeSet<&EventId> = users
		.iter()
		.flat_map(|user| &user.events)
		.map(|(event_id, _)| &**event_id)
		.collect();
	let mut gone = BTreeSet::new();
	for event_id in held {
		if let Ok(pdu) = self.services.timeline.get_pdu(event_id).await
			&& (pdu.is_redacted() || self.services.threads.get_thread_id(&pdu).await != root)
		{
			gone.insert(event_id.to_owned());
		}
	}

	for Held { user_id, before, events, .. } in users {
		let dropped = events
			.iter()
			.filter(|(event_id, _)| gone.contains(event_id))
			.fold((0_u64, 0_u64), |(notify, highlight), (_, held)| {
				(notify.saturating_add(held.0.into()), highlight.saturating_add(held.1.into()))
			});
		if dropped == (0, 0) {
			continue;
		}

		let _lock = self
			.notification_increment_mutex
			.lock(&(room_id.clone(), user_id.clone()))
			.await;
		if self
			.notification_state(&user_id, &room_id, root.as_deref())
			.await
			!= before
		{
			continue;
		}
		let left = (before.0.saturating_sub(dropped.0), before.1.saturating_sub(dropped.1));
		self.put_counts(&user_id, &room_id, root.as_deref(), left);
		// A newer stamp lets sync send a count that dropped to zero.
		match &root {
			| Some(root) => self
				.db
				.roomuserid_lastnotificationread
				.put((&room_id, &user_id, root), *self.services.globals.next_count()),
			| None => self
				.db
				.roomuserid_lastnotificationread
				.put((&room_id, &user_id), *self.services.globals.next_count()),
		}
		// As after a receipt, only a delivery corrects the gateway's badge.
		self.services
			.sending
			.refresh_push_badge(&user_id)
			.await
			.log_err()
			.ok();
	}
}

#[implement(super::Service)]
async fn unread_after(
	&self,
	user: &UserId,
	room: &RoomId,
	read_to: u64,
	thread: &ReceiptThread,
) -> Option<Unread> {
	let pdus = self
		.services
		.timeline
		.pdus(None, room, Some(PduCount::Normal(read_to)))
		.take(RECOUNT_LIMIT + 1);
	futures::pin_mut!(pdus);
	let mut unread = Unread::new();
	let mut visited = 0_usize;
	while let Some(item) = pdus.next().await {
		visited = visited.saturating_add(1);
		if visited > RECOUNT_LIMIT {
			return None;
		}
		let (count, pdu) = item.ok()?;
		if pdu.is_redacted() {
			continue;
		}
		let root = self.services.threads.get_thread_id(&pdu).await;
		if matches!(thread, ReceiptThread::Main) && root.is_some()
			|| matches!(thread, ReceiptThread::Thread(wanted) if root.as_deref() != Some(&**wanted))
		{
			continue;
		}
		let notified: super::Notified = match self
			.db
			.useridcount_notification
			.qry(&(user, count.into_unsigned()))
			.await
			.deserialized()
		{
			| Ok(notified) => notified,
			| Err(error) if error.is_not_found() => continue,
			| Err(_) => return None,
		};
		let (notify, highlight) = super::append::counted(&notified.actions);
		let counts = unread.entry(root).or_default();
		counts.0 = counts.0.saturating_add(u64::from(notify));
		counts.1 = counts.1.saturating_add(u64::from(highlight));
	}
	Some(unread)
}

#[implement(super::Service)]
#[tracing::instrument(level = "debug", skip(self), ret(level = "trace"))]
pub async fn notification_count(&self, user_id: &UserId, room_id: &RoomId) -> u64 {
	let key = (user_id, room_id);
	self.db
		.userroomid_notificationcount
		.qry(&key)
		.await
		.deserialized()
		.unwrap_or(0)
}

/// Return the user's account-wide unread notification count.
///
/// Joined main and thread rows contribute to a saturating total.
#[implement(super::Service)]
#[tracing::instrument(level = "trace", skip(self), ret)]
pub async fn global_notification_count(&self, user_id: &UserId) -> u64 {
	self.db
		.userroomid_notificationcount
		.stream_prefix_raw(&(user_id, Interfix))
		.ignore_err()
		.ready_filter_map(|(key, count)| {
			let count = u64_from_u8(count);

			(count > 0).then(|| (KeyBuf::from(key), count))
		})
		.broad_filter_map(async |(key, count)| {
			let (_, room_id, _): (Ignore, &RoomId, IgnoreAll) =
				deserialize_key(&key).expect("notification count key");

			self.services
				.state_cache
				.is_joined(user_id, room_id)
				.await
				.then_some(count)
		})
		.ready_fold(0_u64, u64::saturating_add)
		.await
}

#[implement(super::Service)]
#[tracing::instrument(level = "debug", skip(self), ret(level = "trace"))]
pub async fn highlight_count(&self, user_id: &UserId, room_id: &RoomId) -> u64 {
	let key = (user_id, room_id);
	self.db
		.userroomid_highlightcount
		.qry(&key)
		.await
		.deserialized()
		.unwrap_or(0)
}

/// Per-thread `(notification, highlight)` counts for one room and user.
/// `Interfix` excludes the legacy 2-tuple main row from the scan; only
/// 3-tuple `(user, room, root)` rows match.
#[implement(super::Service)]
#[tracing::instrument(level = "debug", skip(self))]
pub async fn thread_notification_counts(
	&self,
	user_id: &UserId,
	room_id: &RoomId,
) -> ThreadCounts {
	let prefix = (user_id, room_id, Interfix);
	let notifications = self
		.db
		.userroomid_notificationcount
		.stream_prefix(&prefix)
		.ignore_err()
		.map(notification_kv);

	let highlights = self
		.db
		.userroomid_highlightcount
		.stream_prefix(&prefix)
		.ignore_err()
		.map(highlight_kv);

	select(notifications, highlights)
		.ready_fold(ThreadCounts::default(), merge_thread_count)
		.await
}

fn notification_kv(
	(key, notifications): ((&UserId, &RoomId, OwnedEventId), u64),
) -> (OwnedEventId, (u64, u64)) {
	(key.2, (notifications, 0))
}

fn highlight_kv(
	(key, highlights): ((&UserId, &RoomId, OwnedEventId), u64),
) -> (OwnedEventId, (u64, u64)) {
	(key.2, (0, highlights))
}

fn merge_thread_count(
	mut counts: ThreadCounts,
	(root, (notifications, highlights)): (OwnedEventId, (u64, u64)),
) -> ThreadCounts {
	let entry = counts.entry(root).or_default();
	entry.0 = entry.0.saturating_add(notifications);
	entry.1 = entry.1.saturating_add(highlights);
	counts
}

#[implement(super::Service)]
#[tracing::instrument(level = "debug", skip(self), ret(level = "trace"))]
pub async fn last_notification_read(&self, user_id: &UserId, room_id: &RoomId) -> Result<u64> {
	let key = (room_id, user_id);
	self.db
		.roomuserid_lastnotificationread
		.qry(&key)
		.await
		.deserialized()
}

/// Read the last notification watermark for one thread.
///
/// Uses the same room, user and thread key as thread count resets.
#[implement(super::Service)]
#[tracing::instrument(level = "debug", skip(self), ret(level = "trace"))]
pub async fn last_thread_notification_read(
	&self,
	user_id: &UserId,
	room_id: &RoomId,
	thread_root: &EventId,
) -> Result<u64> {
	let key = (room_id, user_id, thread_root);

	self.db
		.roomuserid_lastnotificationread
		.qry(&key)
		.await
		.deserialized()
}

/// Per-thread last-read counts for one room and user. `Interfix` keeps the
/// scan to 3-tuple `(room, user, root)` rows; the legacy 2-tuple main row
/// is excluded by construction and lives behind `last_notification_read`.
#[implement(super::Service)]
#[tracing::instrument(level = "debug", skip(self))]
pub async fn thread_last_notification_reads(
	&self,
	user_id: &UserId,
	room_id: &RoomId,
) -> ThreadLastReads {
	let prefix = (room_id, user_id, Interfix);
	self.db
		.roomuserid_lastnotificationread
		.stream_prefix(&prefix)
		.ignore_err()
		.map(|((_, _, root), count): ((Ignore, Ignore, OwnedEventId), u64)| (root, count))
		.collect()
		.await
}

#[implement(super::Service)]
pub async fn delete_room_notification_read(&self, room_id: &RoomId) -> Result {
	let key = (room_id, Interfix);
	self.db
		.roomuserid_lastnotificationread
		.keys_prefix_raw(&key)
		.ignore_err()
		.ready_for_each(|key| {
			trace!("Removing key: {key:?}");
			self.db
				.roomuserid_lastnotificationread
				.remove(key);
		})
		.await;

	Ok(())
}
