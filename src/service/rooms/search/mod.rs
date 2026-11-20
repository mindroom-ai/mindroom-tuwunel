#[cfg(test)]
mod tests;

use std::{collections::HashSet, sync::Arc};

use futures::{Stream, StreamExt, TryStreamExt};
use ruma::{RoomId, UserId, api::client::search::search_events::v3::Criteria};
use tuwunel_core::{
	Result, implement, trace,
	utils::{
		IterStream, ReadyExt,
		result::{LogErr, NotFound},
		stream::{TryIgnore, WidebandExt},
	},
};
use tuwunel_database::{KeyBuf, Map, Txn, keyval::Val};
use tuwunel_matrix::{
	PduCount,
	event::{Event, Matches},
};

use crate::rooms::{
	short::ShortRoomId,
	timeline::{PduId, RawPduId},
};

pub struct Service {
	db: Data,
	services: Arc<crate::services::OnceServices>,
}

struct Data {
	tokenids: Arc<Map>,
}

#[derive(Clone, Debug)]
pub struct RoomQuery<'a> {
	pub room_id: &'a RoomId,
	pub user_id: Option<&'a UserId>,
	pub criteria: &'a Criteria,
	pub limit: usize,
	/// Exclusive upper count bound for the returned page.
	pub before: Option<PduCount>,
}

/// A search index key. Lowercasing can lengthen a word past `WORD_MAX_LEN`
/// bytes, so the buffer grows instead of having a fixed capacity.
type TokenId = KeyBuf;

const WORD_MAX_LEN: usize = 50;

/// Words whose lists are read in turn to find the shortest; any further word
/// is opened only on its turn, so a long query holds this many scans at most.
const LOCKSTEP_WORDS: usize = 16;

/// A word's hit list is read whole only while it is at most this many times
/// as long as the hits left; past that, each hit's key is looked up instead.
///
/// On a production-sized index, with the default block cache, a lookup costs
/// about 7 ms of CPU and a scanned posting about 1.7 µs, some 4,000 times
/// less. The first factor, 8, was tuned on a database that fit in the cache,
/// and turned a 0.5 s search of a rare and a common word into 15 s.
pub const PROBE_FACTOR: usize = 4096;

impl crate::Service for Service {
	fn build(args: &crate::Args<'_>) -> Result<Arc<Self>> {
		Ok(Arc::new(Self {
			db: Data { tokenids: args.db["tokenids"].clone() },
			services: args.services.clone(),
		}))
	}

	fn name(&self) -> &str { crate::service::make_name(std::module_path!()) }
}

#[implement(Service)]
pub fn index_pdu(&self, shortroomid: ShortRoomId, pdu_id: &RawPduId, message_body: &str) {
	let items = tokenize(message_body).map(|word| {
		let mut key = shortroomid.to_be_bytes().to_vec();
		key.extend_from_slice(word.as_bytes());
		key.push(0xFF);
		key.extend_from_slice(pdu_id.as_ref()); // TODO: currently we save the room id a second time here

		(key, [])
	});

	Txn::insert(&self.db.tokenids, items).execute();
}

#[implement(Service)]
pub fn deindex_pdu(&self, shortroomid: ShortRoomId, pdu_id: &RawPduId, message_body: &str) {
	let batch = tokenize(message_body).map(|word| {
		let mut key = shortroomid.to_be_bytes().to_vec();
		key.extend_from_slice(word.as_bytes());
		key.push(0xFF);
		key.extend_from_slice(pdu_id.as_ref()); // TODO: currently we save the room id a second time here
		key
	});

	for token in batch {
		self.db.tokenids.remove(&token);
	}
}

/// Searches a room and returns its total hit count and a page of counted events.
///
/// The total is computed before the cursor and event filters. The stream yields
/// visible matches in descending count order, up to the requested room limit.
#[implement(Service)]
pub async fn search_pdus<'a>(
	&'a self,
	query: &'a RoomQuery<'a>,
) -> Result<(usize, impl Stream<Item = (PduCount, impl Event + use<>)> + Send + '_)> {
	let pdu_ids = self.search_pdu_ids(query).await?;

	let filter = &query.criteria.filter;
	let count = pdu_ids.len();
	let pdus = pdu_ids
		.into_iter()
		.stream()
		.ready_filter(|id| {
			query
				.before
				.is_none_or(|before| id.pdu_count().lt(&before))
		})
		.wide_filter_map(async |result_pdu_id: RawPduId| {
			self.services
				.timeline
				.get_pdu_from_id(&result_pdu_id)
				.await
				.ok()
				.map(|pdu| (result_pdu_id.pdu_count(), pdu))
		})
		.ready_filter(|(_, pdu)| !pdu.is_redacted())
		.ready_filter(move |(_, pdu)| filter.matches(pdu))
		.wide_filter_map(async |(count, pdu)| {
			self.services
				.state_accessor
				.user_can_see_event(query.user_id?, &pdu)
				.await
				.then_some((count, pdu))
		})
		.map(|(count, pdu)| (count, pdu.without_private_unless_sender(query.user_id)))
		.take(query.limit);

	Ok((count, pdus))
}

/// The ids of a room's events holding every word of the query, newest first.
///
/// The first `LOCKSTEP_WORDS` words' lists are read one id each in turn until
/// one ends; that shortest list holds the candidates. Each other word's list
/// is then read until it ends or holds `PROBE_FACTOR` times the candidates
/// left plus one. A list that ended is intersected by hash set. Otherwise the
/// ids read are dropped and each candidate's key for that word is looked up
/// instead. A failed lookup, other than a missing key, fails the room's search
/// rather than dropping a hit.
#[implement(Service)]
pub async fn search_pdu_ids(&self, query: &RoomQuery<'_>) -> Result<Vec<RawPduId>> {
	let shortroomid = self
		.services
		.short
		.get_shortroomid(query.room_id)
		.await?;

	let words: Vec<_> = query_words(&query.criteria.search_term).collect();
	let open = |word| {
		let scan = self.search_pdu_ids_query_words(shortroomid, word);
		(word, scan.boxed(), Vec::new())
	};

	let (head, tail) = words.split_at(words.len().min(LOCKSTEP_WORDS));
	let mut scans: Vec<_> = head
		.iter()
		.map(String::as_str)
		.map(&open)
		.collect();

	if scans.is_empty() {
		return Ok(Vec::new());
	}

	let shortest = 'ended: loop {
		for (i, (_, scan, list)) in scans.iter_mut().enumerate() {
			match scan.next().await {
				| Some(id) => list.push(id),
				| None => break 'ended i,
			}
		}
	};

	let (_, _, mut hits) = scans.swap_remove(shortest);
	if hits.is_empty() {
		return Ok(hits);
	}

	let tail = tail.iter().map(String::as_str).map(open);
	for (word, mut scan, mut list) in scans.into_iter().chain(tail) {
		let budget = hits.len().saturating_mul(PROBE_FACTOR);
		while list.len() <= budget
			&& let Some(id) = scan.next().await
		{
			list.push(id);
		}

		hits = if list.len() <= budget {
			intersect_hits(vec![hits, list])
		} else {
			drop(scan);
			let found: Vec<_> = hits
				.into_iter()
				.stream()
				.wide_then(async |id| -> Result<Option<RawPduId>> {
					let key = make_tokenid(shortroomid, word, &id);
					let found = self
						.db
						.tokenids
						.exists(&key)
						.await
						.present()
						.log_err()?;
					Ok(found.map(|()| id))
				})
				.try_collect()
				.await?;

			found.into_iter().flatten().collect()
		};

		if hits.is_empty() {
			break;
		}
	}

	Ok(hits)
}

/// Keeps the ids every word's hit list holds, in the shortest list's order.
///
/// Each list is newest first, so the result is too. The shortest list is
/// checked against a hash set of each other list, which costs the lists' total
/// length. `set::intersection` copied every other list for each candidate of
/// the first, about 3.1 billion ids for one ten-word query, before scanning
/// them. No words give no hits.
fn intersect_hits(mut lists: Vec<Vec<RawPduId>>) -> Vec<RawPduId> {
	lists.sort_unstable_by_key(Vec::len);
	let mut lists = lists.into_iter();
	let mut hits = lists.next().unwrap_or_default();
	for list in lists {
		if hits.is_empty() {
			break;
		}

		let list: HashSet<_> = list.into_iter().collect();
		hits.retain(|id| list.contains(id));
	}

	hits
}

/// Iterate over PduId's containing a word
#[implement(Service)]
fn search_pdu_ids_query_words<'a>(
	&'a self,
	shortroomid: ShortRoomId,
	word: &'a str,
) -> impl Stream<Item = RawPduId> + Send + '_ {
	self.search_pdu_ids_query_word(shortroomid, word)
		.map(move |key| -> RawPduId {
			let key = &key[prefix_len(word)..];
			key.into()
		})
}

/// Iterate over raw database results for a word
#[implement(Service)]
fn search_pdu_ids_query_word(
	&self,
	shortroomid: ShortRoomId,
	word: &str,
) -> impl Stream<Item = Val<'_>> + Send + '_ + use<'_> {
	// rustc says const'ing this not yet stable
	let end_id: RawPduId = PduId { shortroomid, count: PduCount::max() }.into();

	// Newest pdus first
	let end = make_tokenid(shortroomid, word, &end_id);
	let prefix = make_prefix(shortroomid, word);
	self.db
		.tokenids
		.rev_raw_keys_from(&end)
		.ignore_err()
		.ready_take_while(move |key| key.starts_with(&prefix))
}

#[implement(Service)]
pub async fn delete_all_search_tokenids_for_room(&self, room_id: &RoomId) -> Result {
	let Ok(shortroomid) = self.services.short.get_shortroomid(room_id).await else {
		return Ok(());
	};

	let txn = self
		.db
		.tokenids
		.keys_prefix_raw(&shortroomid)
		.ignore_err()
		.ready_fold(self.services.db.txn(), |mut txn, key| {
			trace!("Removing key: {key:?}");
			txn.del_raw(&self.db.tokenids, key);
			txn
		})
		.await;

	txn.execute();

	Ok(())
}

/// The distinct words of a search term, in first-seen order.
///
/// A repeated word holds the same events as its first use, so its hit list is
/// read once.
fn query_words(search_term: &str) -> impl Iterator<Item = String> + Send + '_ {
	let mut seen = HashSet::new();
	tokenize(search_term).filter(move |word| seen.insert(word.clone()))
}

/// Splits a string into tokens used as keys in the search inverted index
///
/// This may be used to tokenize both message bodies (for indexing) or search
/// queries (for querying).
fn tokenize(body: &str) -> impl Iterator<Item = String> + Send + '_ {
	body.split_terminator(|c: char| !c.is_alphanumeric())
		.filter(|s| !s.is_empty())
		.filter(|word| word.len() <= WORD_MAX_LEN)
		.map(str::to_lowercase)
}

fn make_tokenid(shortroomid: ShortRoomId, word: &str, pdu_id: &RawPduId) -> TokenId {
	let mut key = make_prefix(shortroomid, word);
	key.extend_from_slice(pdu_id.as_ref());
	key
}

fn make_prefix(shortroomid: ShortRoomId, word: &str) -> TokenId {
	let mut key = TokenId::new();
	key.extend_from_slice(&shortroomid.to_be_bytes());
	key.extend_from_slice(word.as_bytes());
	key.push(tuwunel_database::SEP);
	key
}

fn prefix_len(word: &str) -> usize {
	size_of::<ShortRoomId>()
		.saturating_add(word.len())
		.saturating_add(1)
}
