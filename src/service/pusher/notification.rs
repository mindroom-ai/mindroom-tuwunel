use std::{collections::BTreeMap, sync::Arc};

use futures::{StreamExt, future::join4, stream::select};
use ruma::{EventId, OwnedEventId, RoomId, UserId, events::receipt::ReceiptThread};
use tuwunel_core::{
	Result, implement,
	matrix::{event::Event, pdu::PduCount},
	trace,
	utils::{
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

/// Retained for internal callers that explicitly read the present main timeline.
#[implement(super::Service)]
pub async fn reset_notification_counts(&self, user_id: &UserId, room_id: &RoomId) {
	self.reset_notification_counts_for_thread(user_id, room_id, None, &ReceiptThread::Main)
		.await;
}

/// Read only through the acknowledged event; an incomplete or raced recount
/// keeps the previous counts. Own sends (`None`) still read their own context.
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
		| _ => {
			let counts = self
				.thread_counts_bounded(user_id, room_id, RECOUNT_LIMIT + 1)
				.await;
			if counts.len() > RECOUNT_LIMIT {
				return;
			}
			std::iter::once(None)
				.chain(counts.into_keys().map(Some))
				.collect()
		},
	};
	// These snapshots and the bounded PDU scan never hold the insertion lock.
	let mut before = BTreeMap::new();
	for root in contexts {
		before.insert(
			root.clone(),
			self.notification_state(user_id, room_id, root.as_deref())
				.await,
		);
	}
	let unread = match read_to {
		| Some(count) => match self
			.unread_after(user_id, room_id, count, thread)
			.await
		{
			| Some(unread) => unread,
			| None => return,
		},
		| None => Unread::new(),
	};
	let _lock = self
		.notification_increment_mutex
		.lock(&(room_id.to_owned(), user_id.to_owned()))
		.await;
	let stamp = self.services.globals.next_count();
	for (root, snapshot) in before {
		if self
			.notification_state(user_id, room_id, root.as_deref())
			.await
			!= snapshot
		{
			continue;
		}
		let after = unread.get(&root).copied().unwrap_or_default();
		let left = (snapshot.0.min(after.0), snapshot.1.min(after.1));
		let key = notification_key(user_id, room_id, root.as_deref());
		if root.is_some() && matches!(thread, ReceiptThread::Unthreaded) && left == (0, 0) {
			self.db.userroomid_notificationcount.remove(&key);
			self.db.userroomid_highlightcount.remove(&key);
		} else {
			self.db
				.userroomid_notificationcount
				.raw_put(&key, left.0);
			self.db
				.userroomid_highlightcount
				.raw_put(&key, left.1);
		}
		if let Some(root) = &root {
			if matches!(thread, ReceiptThread::Unthreaded) {
				self.db
					.roomuserid_lastnotificationread
					.del((room_id, user_id, root));
			} else {
				self.db
					.roomuserid_lastnotificationread
					.put((room_id, user_id, root), *stamp);
			}
		}
	}
	// Publish the room stamp last, including when a concurrent insertion kept
	// the main count unchanged, so every room-wide reset invalidates snapshots.
	if !matches!(thread, ReceiptThread::Thread(_)) {
		self.db
			.roomuserid_lastnotificationread
			.put((room_id, user_id), *stamp);
		self.clear_suppressed_room(user_id, room_id);
	}
}

/// Counts detect concurrent insertions; read stamps prevent ABA when a
/// concurrent receipt lowers counts and a subsequent event raises them again.
#[implement(super::Service)]
async fn notification_state(
	&self,
	user: &UserId,
	room: &RoomId,
	root: Option<&EventId>,
) -> (u64, u64, u64, u64) {
	let key = notification_key(user, room, root);
	let count = async |map: &Arc<Map>| {
		map.get(&key)
			.await
			.deserialized::<u64>()
			.unwrap_or(0)
	};
	join4(
		count(&self.db.userroomid_notificationcount),
		count(&self.db.userroomid_highlightcount),
		async {
			self.last_notification_read(user, room)
				.await
				.unwrap_or(0)
		},
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
	self.thread_counts_bounded(user_id, room_id, usize::MAX)
		.await
}

#[implement(super::Service)]
async fn thread_counts_bounded(
	&self,
	user_id: &UserId,
	room_id: &RoomId,
	limit: usize,
) -> ThreadCounts {
	let prefix = (user_id, room_id, Interfix);
	let notifications = self
		.db
		.userroomid_notificationcount
		.stream_prefix(&prefix)
		.take(limit)
		.ignore_err()
		.map(notification_kv);

	let highlights = self
		.db
		.userroomid_highlightcount
		.stream_prefix(&prefix)
		.take(limit)
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
