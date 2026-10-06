use ruma::server_name;

use super::{MAX_TRANSACTION_EDU_BYTES, edus_within_limit};
use crate::sending::EduBuf;

#[test]
fn edus_past_the_transaction_limit_are_dropped() {
	let edu = |len: usize| EduBuf::from_slice(&vec![b'x'; len]);
	let half = MAX_TRANSACTION_EDU_BYTES / 2;
	let edus = [
		edu(MAX_TRANSACTION_EDU_BYTES.saturating_add(1)),
		edu(half),
		edu(half.saturating_add(1)),
		edu(16),
	];

	let kept: Vec<_> = edus_within_limit(server_name!("remote.example"), edus.iter())
		.map(EduBuf::len)
		.collect();

	assert_eq!(kept, [half, 16]);
}
