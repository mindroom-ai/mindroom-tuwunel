//! Search tokenization and multi-word hit list intersection tests.

#[cfg(target_os = "linux")]
mod timing;

use futures::StreamExt;
use ruma::room_id;
use tuwunel_core::{Result, config::Figment};
use tuwunel_database::SEP;
use tuwunel_matrix::PduCount;

use super::{WORD_MAX_LEN, intersect_hits, make_tokenid, query_words, tokenize};
use crate::{
	rooms::timeline::{PduId, RawPduId},
	test_utils::{fixture, pdu_id},
};

#[test]
fn lowercase_expansion_preserves_the_full_search_key() {
	let body = "İ".repeat(25);
	let word = tokenize(&body).next().expect("one accepted word");
	let pdu_id = pdu_id(42);
	let key = make_tokenid(1, &word, &pdu_id);
	let expected: Vec<_> = 1_u64
		.to_be_bytes()
		.into_iter()
		.chain("i\u{307}".repeat(25).bytes())
		.chain([SEP])
		.chain(pdu_id.as_bytes().iter().copied())
		.collect();

	assert_eq!(body.len(), WORD_MAX_LEN);
	assert!(word.len() > WORD_MAX_LEN);
	assert_eq!(key.as_slice(), expected);
	assert!(key.spilled());
}

#[tokio::test]
async fn lowercase_expansion_remains_searchable() -> Result {
	let Some(fixture) = fixture(Figment::new()).await? else {
		return Ok(());
	};

	let services = &fixture.services;
	let room = room_id!("!search:localhost");
	let shortroomid = services
		.short
		.get_or_create_shortroomid(room)
		.await;

	let pdu = pdu_id(42);
	let body = "İ".repeat(25);
	let word = tokenize(&body).next().expect("one accepted word");

	services
		.search
		.index_pdu(shortroomid, &pdu, &body);

	let found: Vec<_> = services
		.search
		.search_pdu_ids_query_words(shortroomid, &word)
		.collect()
		.await;

	assert_eq!(found, [pdu]);
	services
		.search
		.deindex_pdu(shortroomid, &pdu, &body);

	let remaining = services
		.search
		.search_pdu_ids_query_words(shortroomid, &word)
		.count()
		.await;

	assert_eq!(remaining, 0);

	Ok(())
}

/// The hits are those in every list, newest first, whichever list is shortest.
#[test]
fn hits_are_in_every_list_newest_first() {
	let lists = vec![ids(&[9, 7, 5, 3, 1]), ids(&[7, 3]), ids(&[9, 7, 3, 2])];

	assert_eq!(intersect_hits(lists), ids(&[7, 3]));
	assert_eq!(intersect_hits(vec![ids(&[2, 1]); 2]), ids(&[2, 1]));
	assert_eq!(intersect_hits(vec![ids(&[2, 1])]), ids(&[2, 1]));
	assert_eq!(intersect_hits(vec![ids(&[2]), Vec::new()]), ids(&[]));
	assert_eq!(intersect_hits(Vec::new()), ids(&[]));
}

/// A repeated word, in any case, is read once, where it first appears.
#[test]
fn query_words_are_distinct_in_first_seen_order() {
	let words: Vec<_> = query_words("Go far to go, fast to GO!").collect();

	assert_eq!(words, ["go", "far", "to", "fast"]);
	assert_eq!(query_words("?! ...").count(), 0);
}

fn ids(counts: &[u64]) -> Vec<RawPduId> { counts.iter().copied().map(id).collect() }

fn id(count: u64) -> RawPduId {
	PduId {
		shortroomid: 7,
		count: PduCount::Normal(count),
	}
	.into()
}
