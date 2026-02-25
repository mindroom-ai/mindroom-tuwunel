use std::{collections::HashSet as Set, iter::once, ops::Deref};

use futures::{
	Future, Stream, StreamExt,
	stream::{FuturesUnordered, unfold},
};
use ruma::OwnedEventId;
use tuwunel_core::{
	Result, implement,
	matrix::{Event, pdu::AuthEvents},
	smallvec::SmallVec,
	utils::stream::IterStream,
};

#[derive(Default)]
struct Global<Fut> {
	subgraph: Set<OwnedEventId>,
	seen: Set<OwnedEventId>,
	todo: FuturesUnordered<Fut>,
}

#[derive(Default, Debug)]
struct Local {
	path: Path,
	stack: Stack,
}

type Path = SmallVec<[OwnedEventId; PATH_INLINE]>;
type Stack = SmallVec<[Frame; STACK_INLINE]>;
type Frame = AuthEvents;

const PATH_INLINE: usize = 16;
const STACK_INLINE: usize = 16;

#[tracing::instrument(name = "subgraph_dfs", level = "debug", skip_all)]
pub(super) fn conflicted_subgraph_dfs<Fetch, Fut, Pdu>(
	conflicted_set: &Vec<&OwnedEventId>,
	fetch: &Fetch,
) -> impl Stream<Item = OwnedEventId> + Send
where
	Fetch: Fn(OwnedEventId) -> Fut + Sync,
	Fut: Future<Output = Result<Pdu>> + Send,
	Pdu: Event,
{
	let mut state = Global {
		subgraph: Default::default(),
		seen: Default::default(),
		todo: FuturesUnordered::<_>::new(),
	};

	let (todo, initial): (Vec<(Local, Option<OwnedEventId>)>, Vec<Path>) = conflicted_set
		.iter()
		.map(Deref::deref)
		.cloned()
		.map(Local::new)
		.filter_map(Local::pop)
		.map(|(local, event_id)| local.eval(&mut state, conflicted_set, event_id))
		.map(|(local, event_id, path)| ((local, event_id), path))
		.unzip();

	state.todo.extend(
		todo.into_iter()
			.filter_map(|(local, event_id)| event_id.map(|event_id| (local, event_id)))
			.map(|(local, event_id)| local.push(fetch, event_id)),
	);

	unfold(state, |mut state| async {
		let outputs = state
			.todo
			.next()
			.await?
			.pop()
			.map(|(local, event_id)| local.eval(&mut state, conflicted_set, event_id))
			.map(|(local, next_id, outputs)| {
				if let Some(next_id) = next_id {
					state.todo.push(local.push(fetch, next_id));
				}

				outputs
			})
			.into_iter()
			.flatten()
			.stream();

		Some((outputs, state))
	})
	.flatten()
	.chain(initial.into_iter().flatten().stream())
}

#[implement(Local)]
fn new(conflicted_event_id: OwnedEventId) -> Self {
	Self {
		path: once(conflicted_event_id.clone()).collect(),
		stack: once(once(conflicted_event_id).collect()).collect(),
	}
}

#[implement(Local)]
fn eval<Fut>(
	mut self,
	state: &mut Global<Fut>,
	conflicted_event_ids: &Vec<&OwnedEventId>,
	event_id: OwnedEventId,
) -> (Self, Option<OwnedEventId>, Path) {
	let Global { subgraph, seen, .. } = state;

	if subgraph.contains(&event_id) {
		if self.path.len() <= 1 {
			self.path.pop();
			return (self, None, Path::new());
		}

		let path = self
			.path
			.iter()
			.filter(|&event_id| subgraph.insert(event_id.clone()))
			.cloned()
			.collect();

		self.path.pop();
		return (self, None, path);
	}

	if !seen.insert(event_id.clone()) {
		return (self, None, Path::new());
	}

	if self.path.len() > 1
		&& conflicted_event_ids
			.binary_search(&&event_id)
			.is_ok()
	{
		let path = self
			.path
			.iter()
			.filter(|&event_id| subgraph.insert(event_id.clone()))
			.cloned()
			.collect();

		return (self, Some(event_id), path);
	}

	(self, Some(event_id), Path::new())
}

#[implement(Local)]
async fn push<Fetch, Fut, Pdu>(mut self, fetch: &Fetch, event_id: OwnedEventId) -> Self
where
	Fetch: Fn(OwnedEventId) -> Fut + Sync,
	Fut: Future<Output = Result<Pdu>> + Send,
	Pdu: Event,
{
	if let Ok(auth_events) = fetch(event_id)
		.await
		.map(|event| event.auth_events_into().into_iter().collect())
	{
		self.stack.push(auth_events);
	}

	self
}

#[implement(Local)]
fn pop(mut self) -> Option<(Self, OwnedEventId)> {
	while self.stack.last().is_some_and(Frame::is_empty) {
		self.stack.pop();
		self.path.pop();
	}

	self.stack
		.last_mut()
		.and_then(Frame::pop)
		.inspect(|event_id| self.path.push(event_id.clone()))
		.map(move |event_id| (self, event_id))
}
