//! Hit list intersection behind multi-word room searches.

#[cfg(target_os = "linux")]
mod timing;

use tuwunel_matrix::PduCount;

use super::{WORD_MAX_LEN, intersect_hits, make_tokenid, query_words, tokenize};
use crate::rooms::timeline::{PduId, RawPduId};

/// The hits are those in every list, newest first, whichever list is shortest.
#[test]
fn hits_are_in_every_list_newest_first() {
	let lists = vec![ids(&[9, 7, 5, 3, 1]), ids(&[7, 3]), ids(&[9, 7, 3, 2])];

	assert_eq!(intersect_hits(lists), ids(&[7, 3]));
	assert_eq!(intersect_hits(vec![ids(&[2, 1]); 2]), ids(&[2, 1]));
	assert_eq!(intersect_hits(vec![ids(&[2, 1])]), ids(&[2, 1]));
	assert!(intersect_hits(vec![ids(&[2]), Vec::new()]).is_empty());
	assert!(intersect_hits(Vec::new()).is_empty());
}

/// A repeated word, in any case, is read once, where it first appears.
#[test]
fn query_words_are_distinct_in_first_seen_order() {
	let words: Vec<_> = query_words("Go far to go, fast to GO!").collect();

	assert_eq!(words, ["go", "far", "to", "fast"]);
	assert_eq!(query_words("?! ...").count(), 0);
}

/// A word that lowercasing lengthens past `WORD_MAX_LEN` bytes still gets the
/// full key `index_pdu` writes, for normal and backfilled ids.
#[test]
fn tokenids_fit_words_lowercasing_lengthens() {
	let body = "\u{130}".repeat(WORD_MAX_LEN / 2);
	let word = tokenize(&body).next().expect("one word");
	assert_eq!(body.len(), WORD_MAX_LEN);
	assert_eq!(word.len(), WORD_MAX_LEN / 2 * 3);

	for count in [PduCount::Normal(9), PduCount::Backfilled(-9)] {
		let pdu_id: RawPduId = PduId { shortroomid: 7, count }.into();
		let expected =
			[&7_u64.to_be_bytes()[..], word.as_bytes(), &[0xFF], pdu_id.as_ref()].concat();

		assert_eq!(make_tokenid(7, &word, &pdu_id).as_slice(), expected);
	}
}

fn ids(counts: &[u64]) -> Vec<RawPduId> { counts.iter().copied().map(id).collect() }

fn id(count: u64) -> RawPduId {
	PduId {
		shortroomid: 7,
		count: PduCount::Normal(count),
	}
	.into()
}
