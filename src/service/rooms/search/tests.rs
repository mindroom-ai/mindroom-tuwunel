//! Hit list intersection behind multi-word room searches.

#[cfg(target_os = "linux")]
mod timing;

use tuwunel_matrix::PduCount;

use super::intersect_hits;
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

fn ids(counts: &[u64]) -> Vec<RawPduId> { counts.iter().copied().map(id).collect() }

fn id(count: u64) -> RawPduId {
	PduId {
		shortroomid: 7,
		count: PduCount::Normal(count),
	}
	.into()
}
