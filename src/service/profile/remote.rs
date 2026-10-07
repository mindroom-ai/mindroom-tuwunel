use futures::TryStreamExt;
use ruma::{
	UserId,
	api::federation::query::get_profile_information::v1::{Request, Response},
	profile::ProfileFieldName,
};
use serde_json::Value;
use tuwunel_core::{Err, Result, implement, smallvec::SmallVec, utils::stream::TryReadyExt};

use super::{MAX_PROFILE_SIZE, Propagation, Service, check_profile_key};

type Removed = SmallVec<[ProfileFieldName; 1]>;

type Fields = Vec<(ProfileFieldName, Option<Value>)>;

/// Largest profile answer read from a remote server: four times the profile
/// size limit, which leaves room for characters a server escapes as `\uXXXX`.
const MAX_PROFILE_RESPONSE_BYTES: usize = 4 * MAX_PROFILE_SIZE;

/// The most fields a served profile may hold.
///
/// Every lookup refetches the profile and rewrites each field it serves or
/// drops, so this bounds the work of one lookup; real profiles hold a handful.
pub(super) const MAX_SERVED_PROFILE_FIELDS: usize = 100;

/// Replaces a remote user's cached profile with the one their server serves.
///
/// A cached field missing from the response is removed, so a value the remote
/// user has since deleted stops reaching clients. Returns the names of the
/// removed fields.
#[implement(Service)]
#[tracing::instrument(
	level = "debug",
	skip_all,
	fields(
		%user_id,
	),
)]
pub async fn mirror_remote_profile(&self, user_id: &UserId) -> Result<Removed> {
	assert!(
		!self.services.globals.user_is_local(user_id),
		"mirror remote profile called with a local user"
	);

	let response = self.request_remote_profile(user_id).await?;

	self.mirror_profile(user_id, response).await
}

/// Requests a remote user's complete profile from their server.
#[implement(Service)]
pub(super) async fn request_remote_profile(&self, user_id: &UserId) -> Result<Response> {
	let client = &self.services.client.federation;
	let request = Request { user_id: user_id.to_owned(), field: None };

	self.services
		.federation
		.execute_on(client, user_id.server_name(), request, MAX_PROFILE_RESPONSE_BYTES)
		.await
}

/// Stores a profile response as the user's complete cached profile.
///
/// Every returned field is written and every cached field the response omits
/// is deleted in one logged write under the profile lock, so no concurrent
/// write interleaves and connected clients see the removals. A response with a
/// field name outside the MSC4133 grammar, over the 64 KiB cap, or with more
/// than 100 fields is refused and the cache is left as it was.
#[implement(Service)]
pub(super) async fn mirror_profile(
	&self,
	user_id: &UserId,
	response: Response,
) -> Result<Removed> {
	check_served_profile(&response)?;

	let profile_lock = self.mutex.lock(user_id).await;
	let removed: Removed = self
		.try_profile_field_names(user_id)
		.ready_try_filter(|name| response.get(name.as_str()).is_none())
		.try_collect()
		.await?;

	let fields: Fields = response
		.into_iter()
		.map(|(name, value)| (name.into(), Some(value)))
		.chain(removed.iter().cloned().map(|name| (name, None)))
		.collect();

	self.set_profile_keys_locked(&profile_lock, user_id, &fields, Some(Propagation::None))
		.await?;

	Ok(removed)
}

/// Checks a served profile against the field-name and size limits of a local
/// one, and against the field-count cap.
fn check_served_profile(response: &Response) -> Result {
	if response.iter().len() > MAX_SERVED_PROFILE_FIELDS {
		return Err!(Request(ProfileTooLarge("Profile has more than 100 fields.")));
	}

	response
		.iter()
		.try_for_each(|(name, _)| check_profile_key(name))?;

	if serde_json::to_vec(&response.data)?.len() > MAX_PROFILE_SIZE {
		return Err!(Request(ProfileTooLarge("Profile exceeds the maximum size of 64 KiB.")));
	}

	Ok(())
}
