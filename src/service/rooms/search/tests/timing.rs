//! The cost of intersecting production-sized hit lists.
//!
//! Linux only, where the CPU time measured is this thread's alone.

use std::time::{Duration, Instant};

use tuwunel_core::utils::sys::thread_usage;

use super::{super::intersect_hits, id};
use crate::rooms::timeline::RawPduId;

/// Messages in the synthetic room, numbered by count from one.
const MESSAGES: u64 = 400_000;

/// One message in this many holds every word.
const SHARED_EVERY: u64 = 4_000;

/// Hits per word of a ten-word sentence of common words, in order, in one
/// production room. The repeated words repeat their lists.
const WORD_HITS: [u64; 10] =
	[3_837, 171_950, 32_715, 84_557, 1_267, 206_178, 84_557, 1_496, 206_178, 20_941];

/// Production-sized hit lists, already read, intersect fast.
///
/// The words' hits are spread pseudo-randomly over the room, and every
/// `SHARED_EVERY`th message holds them all. The old intersection copied every
/// other list for each candidate of the first, about 3.1 billion ids for these
/// sizes, and the whole search request took about 47 seconds in production;
/// on these lists the old intersection took 35 to 49 seconds of CPU. The bound,
/// 5 seconds, is on the Linux thread CPU time of the intersection alone, which
/// excludes time descheduled; it is not the endpoint's latency, since reading
/// the hit lists and loading and filtering events are not measured.
#[test]
fn production_sized_hit_lists_intersect_quickly() {
	let lists = WORD_HITS
		.iter()
		.map(|&hits| newest_first(|count| holds(hits, count)))
		.collect();

	let expected = newest_first(|count| WORD_HITS.iter().all(|&hits| holds(hits, count)));

	let (started, cpu_started) = (Instant::now(), cpu_time());
	let hits = intersect_hits(lists);
	let (elapsed, cpu) = (started.elapsed(), cpu_time().saturating_sub(cpu_started));

	eprintln!("intersected production-sized hit lists in {elapsed:?}, {cpu:?} of CPU");
	assert!(expected.len() >= 100, "every shared message is a hit");
	assert_eq!(hits, expected);
	assert!(cpu < Duration::from_secs(5), "the intersection took {cpu:?} of CPU");
}

fn newest_first(holds: impl Fn(u64) -> bool) -> Vec<RawPduId> {
	(1..=MESSAGES)
		.rev()
		.filter(|&count| holds(count))
		.map(id)
		.collect()
}

/// Whether a message holds the word with about `hits` hits in the room.
fn holds(hits: u64, count: u64) -> bool {
	count.is_multiple_of(SHARED_EVERY) || mix(count ^ hits.rotate_left(32)) % MESSAGES < hits
}

/// The SplitMix64 finalizer, a fixed pseudo-random spread.
fn mix(mut x: u64) -> u64 {
	x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
	x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
	x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
	x ^ (x >> 31)
}

/// The user and system time this thread has run for.
fn cpu_time() -> Duration {
	let usage = thread_usage().expect("thread resource usage");

	[usage.user_time(), usage.system_time()]
		.into_iter()
		.map(|time| {
			let secs = time.tv_sec().try_into().expect("CPU seconds");
			let micros = time
				.tv_usec()
				.try_into()
				.expect("CPU microseconds");

			Duration::from_secs(secs).saturating_add(Duration::from_micros(micros))
		})
		.sum()
}
