//! Custom ephemeral room events (MSC2477), held in memory only.
//!
//! Each event takes a global count, as typing does, so `/sync` can relay it in
//! the window between a request's `since` and its `next_batch`. Nothing else is
//! stored: a restart drops every buffered event.

#[cfg(test)]
mod tests;

use std::{
	collections::{HashMap, VecDeque, hash_map},
	hash::{BuildHasher, RandomState},
	ops::Deref,
	sync::{Arc, RwLock},
};

use ruma::{
	DeviceId, MilliSecondsSinceUnixEpoch, OwnedDeviceId, OwnedRoomId, OwnedTransactionId,
	OwnedUserId, RoomId, TransactionId, UInt, UserId, api::client::filter::RoomEventFilter,
};
use serde_json::value::RawValue as RawJsonValue;
use tokio::sync::broadcast;
use tuwunel_core::{Result, trace, utils::millis_since_unix_epoch};

/// Most events one room holds; the oldest is evicted first. That is nearly 7 s
/// of one sender at the rate limit, longer than the 5 s the MindRoom runtime
/// leaves between a reply's snapshots; `MAX_BYTES` binds first only while
/// events average over 1 KiB.
const MAX_EVENTS: usize = 4_096;

/// Most bytes of content, event types, and transaction IDs one room holds; the
/// oldest events are evicted first. One sender's 50 replies put at most about
/// 800 KiB a second into a room: a snapshot of at most 64 KiB (the largest
/// body) from each every 5 s, 640 KiB a second, and about 500 deltas of some
/// 300 bytes. So this holds at least the 5 s between a reply's snapshots even
/// when every snapshot is as large as it may be, and a sync up to 5 s behind
/// finds every reply's latest snapshot.
const MAX_BYTES: usize = 4 * 1_048_576;

/// Age in milliseconds after which an event is no longer delivered.
const TTL_MS: u64 = 30_000;

/// Length in milliseconds of one rate-limit window.
const RATE_WINDOW_MS: u64 = 1_000;

/// Most events one sender may send to one room in a rate-limit window: one
/// agent streaming 50 replies at the MindRoom runtime's 10 frames a second,
/// with its sub-agent frames and room to spare.
const RATE_LIMIT: usize = 600;

/// Milliseconds a write waits after a sweep before sweeping every room again.
const SWEEP_INTERVAL_MS: u64 = 60_000;

pub struct Service {
	services: Arc<crate::services::OnceServices>,
	buffer: RwLock<Buffer>,
	/// Announces each buffered event as its room and event type.
	pub ephemeral_update_sender: broadcast::Sender<(OwnedRoomId, String)>,
}

/// One buffered event, as `/sync` delivers it.
#[derive(Clone, Debug)]
pub struct BufferedEphemeral {
	pub count: u64,
	pub event_type: String,
	pub sender: OwnedUserId,
	pub origin_server_ts: MilliSecondsSinceUnixEpoch,
	pub content: Box<RawJsonValue>,
}

/// What became of a send.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SendOutcome {
	/// The event was buffered and announced.
	Accepted,

	/// The same transaction is already buffered, so nothing changed.
	Duplicate,

	/// The sender's window for this room is full for `retry_after_ms` more.
	RateLimited {
		retry_after_ms: u64,
	},
}

#[derive(Default)]
struct Buffer {
	rooms: HashMap<OwnedRoomId, Room>,
	rates: HashMap<(OwnedRoomId, OwnedUserId), Window>,
	last_sweep: u64,
	txn_hasher: RandomState,
}

/// One room's events in count order, with the sum of their sizes and how many
/// of them hold each transaction hash.
#[derive(Default)]
struct Room {
	entries: VecDeque<Entry>,
	bytes: usize,
	txns: HashMap<u64, usize>,
	/// Whether an event may be buffered behind a newer one, as after the clock
	/// steps back, so an expired event may sit behind a live one. The flag stays
	/// set after the caps evict that event, until an expiry finds the order
	/// restored.
	unordered: bool,
}

struct Entry {
	event: BufferedEphemeral,
	device_id: Option<OwnedDeviceId>,
	txn_id: OwnedTransactionId,
	txn_hash: u64,
}

/// The events one sender sent to one room in the window starting at `start`.
struct Window {
	start: u64,
	events: usize,
}

/// An event offered to the buffer.
struct New<'a> {
	room_id: &'a RoomId,
	sender: &'a UserId,
	device_id: Option<&'a DeviceId>,
	event_type: &'a str,
	txn_id: &'a TransactionId,
	content: Box<RawJsonValue>,
}

impl crate::Service for Service {
	fn build(args: &crate::Args<'_>) -> Result<Arc<Self>> {
		Ok(Arc::new(Self {
			services: args.services.clone(),
			buffer: RwLock::default(),
			ephemeral_update_sender: broadcast::channel(100).0,
		}))
	}

	fn name(&self) -> &str { crate::service::make_name(std::module_path!()) }
}

impl Service {
	/// Buffers one event and wakes the syncs watching its room.
	///
	/// The count is taken under the buffer lock and retires only after the
	/// lock is released, so a sync whose `next_batch` covers the count finds
	/// the event.
	pub fn send(
		&self,
		room_id: &RoomId,
		sender: &UserId,
		device_id: Option<&DeviceId>,
		event_type: &str,
		txn_id: &TransactionId,
		content: Box<RawJsonValue>,
	) -> SendOutcome {
		let new = New {
			room_id,
			sender,
			device_id,
			event_type,
			txn_id,
			content,
		};

		let mut buffer = self.buffer.write().expect("locked for writing");
		let next_count = || self.services.globals.next_count();
		let count = match buffer.send(millis_since_unix_epoch(), new, next_count) {
			| Ok(count) => count,
			| Err(outcome) => return outcome,
		};

		drop(buffer);
		drop(count);

		if self
			.ephemeral_update_sender
			.send((room_id.to_owned(), event_type.to_owned()))
			.is_err()
		{
			trace!("no sync is watching for ephemeral events");
		}

		SendOutcome::Accepted
	}

	/// Unexpired events of a room with `since < count <= until` whose types
	/// `filter` opts into, oldest first.
	#[must_use]
	pub fn events_between(
		&self,
		room_id: &RoomId,
		since: u64,
		until: u64,
		filter: &RoomEventFilter,
	) -> Vec<BufferedEphemeral> {
		self.buffer
			.read()
			.expect("locked for reading")
			.between(room_id, since, until, filter, millis_since_unix_epoch())
	}
}

/// Whether a sync filter opts into ephemeral events of `event_type`.
///
/// `room.ephemeral.types` must list the type, and `not_types` must not. Unlike
/// standard filter semantics, an absent `types` opts into nothing, so syncs
/// that never asked for custom ephemeral events neither get them nor wake for
/// them.
#[must_use]
pub fn opts_into(filter: &RoomEventFilter, event_type: &str) -> bool {
	let matches = |pattern: &String| type_matches(pattern, event_type);

	filter
		.types
		.as_ref()
		.is_some_and(|types| types.iter().any(matches))
		&& !filter.not_types.iter().any(matches)
}

/// Matrix filter type matching, where each `*` matches any run of characters.
fn type_matches(pattern: &str, event_type: &str) -> bool {
	let mut parts = pattern.split('*');
	let first = parts.next().unwrap_or_default();
	let Some(mut rest) = event_type.strip_prefix(first) else {
		return false;
	};

	let Some(last) = parts.next_back() else {
		return rest.is_empty();
	};

	for part in parts {
		let Some((_, after)) = rest.split_once(part) else {
			return false;
		};

		rest = after;
	}

	rest.ends_with(last)
}

impl Buffer {
	/// Buffers an event under the count `next_count` takes, or refuses it.
	///
	/// A refused event takes no count. An accepted one returns its count, so
	/// the caller retires it after releasing the buffer.
	fn send<Count, Next>(
		&mut self,
		now: u64,
		new: New<'_>,
		next_count: Next,
	) -> Result<Count, SendOutcome>
	where
		Count: Deref<Target = u64>,
		Next: FnOnce() -> Count,
	{
		if now.saturating_sub(self.last_sweep) > SWEEP_INTERVAL_MS {
			self.sweep(now);
		}

		let txn_hash =
			self.txn_hasher
				.hash_one((new.event_type, new.sender, new.device_id, new.txn_id));

		let room = self
			.rooms
			.entry(new.room_id.to_owned())
			.or_default();

		// A retried transaction that already landed is answered before the rate
		// window, so a retry never turns an accepted send into a refusal. Only a
		// send whose hash is buffered scans, comparing hashes first.
		room.expire(now);
		if room.txns.contains_key(&txn_hash)
			&& room
				.entries
				.iter()
				.any(|entry| entry.txn_hash == txn_hash && entry.is(&new))
		{
			return Err(SendOutcome::Duplicate);
		}

		let window = self
			.rates
			.entry((new.room_id.to_owned(), new.sender.to_owned()))
			.or_insert(Window { start: now, events: 0 });

		let window_end = window.start.saturating_add(RATE_WINDOW_MS);
		if now >= window_end {
			*window = Window { start: now, events: 0 };
		} else if window.events >= RATE_LIMIT {
			return Err(SendOutcome::RateLimited {
				retry_after_ms: window_end.saturating_sub(now),
			});
		}

		window.events = window.events.saturating_add(1);

		let count = next_count();
		room.push(Entry {
			event: BufferedEphemeral {
				count: *count,
				event_type: new.event_type.to_owned(),
				sender: new.sender.to_owned(),
				origin_server_ts: MilliSecondsSinceUnixEpoch(UInt::new_saturating(now)),
				content: new.content,
			},
			device_id: new.device_id.map(ToOwned::to_owned),
			txn_id: new.txn_id.to_owned(),
			txn_hash,
		});

		Ok(count)
	}

	fn between(
		&self,
		room_id: &RoomId,
		since: u64,
		until: u64,
		filter: &RoomEventFilter,
		now: u64,
	) -> Vec<BufferedEphemeral> {
		self.rooms
			.get(room_id)
			.into_iter()
			.flat_map(|room| room.entries.iter())
			.filter(|entry| entry.event.count > since && entry.event.count <= until)
			.filter(|entry| !entry.expired(now))
			.filter(|entry| opts_into(filter, &entry.event.event_type))
			.map(|entry| entry.event.clone())
			.collect()
	}

	/// Drops expired events of every room, rooms left empty, and past windows.
	fn sweep(&mut self, now: u64) {
		self.rooms.retain(|_, room| {
			room.expire(now);
			!room.entries.is_empty()
		});

		self.rates
			.retain(|_, window| now < window.start.saturating_add(RATE_WINDOW_MS));

		self.last_sweep = now;
	}
}

impl Room {
	/// Appends an event, evicting the oldest beyond the count or byte cap. The
	/// count cap makes room first, so the queue never grows past it.
	fn push(&mut self, entry: Entry) {
		self.unordered |= self
			.entries
			.back()
			.is_some_and(|last| entry.sent() < last.sent());

		while self.entries.len() >= MAX_EVENTS {
			drop(self.pop_oldest());
		}

		self.bytes = self.bytes.saturating_add(entry.bytes());
		let held = self.txns.entry(entry.txn_hash).or_default();
		*held = held.saturating_add(1);
		self.entries.push_back(entry);

		while self.bytes > MAX_BYTES {
			if self.pop_oldest().is_none() {
				break;
			}
		}
	}

	/// Drops expired events. They are the oldest ones unless the clock stepped
	/// back; then every event is checked until the order holds again.
	fn expire(&mut self, now: u64) {
		if self.unordered {
			self.entries.retain(|entry| {
				let live = !entry.expired(now);
				if !live {
					forget(&mut self.bytes, &mut self.txns, entry);
				}

				live
			});

			self.unordered = !self.entries.iter().map(Entry::sent).is_sorted();
			return;
		}

		while self
			.entries
			.front()
			.is_some_and(|entry| entry.expired(now))
		{
			drop(self.pop_oldest());
		}
	}

	fn pop_oldest(&mut self) -> Option<Entry> {
		let evicted = self.entries.pop_front()?;
		forget(&mut self.bytes, &mut self.txns, &evicted);

		Some(evicted)
	}
}

/// Takes a dropped event out of its room's byte sum and transaction index.
fn forget(bytes: &mut usize, txns: &mut HashMap<u64, usize>, entry: &Entry) {
	*bytes = bytes.saturating_sub(entry.bytes());
	if let hash_map::Entry::Occupied(mut held) = txns.entry(entry.txn_hash) {
		match held.get().saturating_sub(1) {
			| 0 => drop(held.remove()),
			| left => drop(held.insert(left)),
		}
	}
}

impl Entry {
	fn bytes(&self) -> usize {
		self.event
			.content
			.get()
			.len()
			.saturating_add(self.event.event_type.len())
			.saturating_add(self.txn_id.as_str().len())
	}

	fn sent(&self) -> u64 { self.event.origin_server_ts.get().into() }

	fn expired(&self, now: u64) -> bool { now.saturating_sub(self.sent()) > TTL_MS }

	/// Whether this is the same transaction as `new`.
	fn is(&self, new: &New<'_>) -> bool {
		self.event.event_type == new.event_type
			&& self.event.sender == new.sender
			&& self.device_id.as_deref() == new.device_id
			&& self.txn_id == new.txn_id
	}
}
