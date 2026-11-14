use axum::extract::State;
use futures::{StreamExt, TryFutureExt, join};
use ruma::{MilliSecondsSinceUnixEpoch, OwnedUserId, UInt, api::Direction};
use synapse_admin_api::users::list_users::{
	v2::{self, UserMinorDetails},
	v3,
};
use tuwunel_core::{
	Result,
	utils::{
		ReadyExt, TryFutureExtExt,
		math::{ruma_from_usize, usize_from_ruma},
		stream::WidebandExt,
	},
};

use super::user_type;
use crate::{Ruma, require_admin};

/// The `deactivated` query filter, whose semantics differ between v2 and v3.
#[derive(Clone, Copy)]
enum DeactivatedFilter {
	/// Include both active and deactivated users (v3 absent or v2 `true`).
	Any,

	/// Only deactivated users (v3 `true`).
	Only,

	/// Exclude deactivated users (v2 absent/false, v3 `false`).
	Exclude,
}

/// Filter and pagination parameters shared by the v2 and v3 list endpoints.
struct ListParams<'a> {
	from: UInt,
	limit: Option<UInt>,
	name: Option<&'a str>,
	user_id: Option<&'a str>,
	admins: Option<bool>,
	locked: bool,
	deactivated: DeactivatedFilter,
	dir: Option<Direction>,
	not_user_type: &'a [String],
}

/// # `GET /_synapse/admin/v2/users`
pub(crate) async fn admin_list_users_v2_route(
	State(services): State<crate::State>,
	body: Ruma<v2::Request>,
) -> Result<v2::Response> {
	require_admin(&services, body.sender_user()).await?;

	let deactivated = match body.deactivated {
		| true => DeactivatedFilter::Any,
		| false => DeactivatedFilter::Exclude,
	};

	let params = ListParams {
		from: body.from,
		limit: body.limit,
		name: body.name.as_deref(),
		user_id: body.user_id.as_deref(),
		admins: body.admins,
		locked: body.locked,
		deactivated,
		dir: body.dir,
		not_user_type: &body.not_user_type,
	};

	let (users, next_token, total) = list_users(services, &params).await;

	Ok(v2::Response { users, next_token, total })
}

/// # `GET /_synapse/admin/v3/users`
pub(crate) async fn admin_list_users_v3_route(
	State(services): State<crate::State>,
	body: Ruma<v3::Request>,
) -> Result<v3::Response> {
	require_admin(&services, body.sender_user()).await?;

	let deactivated = match body.deactivated {
		| None => DeactivatedFilter::Any,
		| Some(true) => DeactivatedFilter::Only,
		| Some(false) => DeactivatedFilter::Exclude,
	};

	let params = ListParams {
		from: body.from,
		limit: body.limit,
		name: body.name.as_deref(),
		user_id: body.user_id.as_deref(),
		admins: body.admins,
		locked: body.locked,
		deactivated,
		dir: body.dir,
		not_user_type: &body.not_user_type,
	};

	let (users, next_token, total) = list_users(services, &params).await;

	Ok(v3::Response { users, next_token, total })
}

/// Returns the filtered, name-ordered and paginated user page, the `next_token`
/// (present only while the page does not reach the end of the filtered set) and
/// the filtered total. `order_by` beyond `name` is not backed by stored fields,
/// so the name ordering (reversed for `dir=b`) is the only one applied.
async fn list_users(
	services: crate::State,
	params: &ListParams<'_>,
) -> (Vec<UserMinorDetails>, Option<String>, UInt) {
	let from = usize_from_ruma(params.from);
	let limit = params.limit.map_or(100, usize_from_ruma);
	let dir = params.dir.unwrap_or(Direction::Forward);

	let name_lower = params
		.name
		.map(str::to_lowercase)
		.filter(|name| !name.is_empty());
	let user_id_lower = params.user_id.map(str::to_lowercase);

	let mut matched: Vec<UserMinorDetails> = services
		.users
		.stream()
		.ready_filter(|user_id| services.globals.user_is_local(user_id))
		.map(ToOwned::to_owned)
		.wide_filter_map(async |user_id| {
			user_minor_details(
				services,
				user_id,
				user_id_lower.as_deref(),
				name_lower.as_deref(),
				params,
			)
			.await
		})
		.collect()
		.await;

	if matches!(dir, Direction::Backward) {
		matched.reverse();
	}

	let matched_count = matched.len();
	let total = ruma_from_usize(matched_count);

	let page: Vec<UserMinorDetails> = matched
		.into_iter()
		.skip(from)
		.take(limit)
		.collect();

	let end = from.saturating_add(page.len());
	let next_token = (end < matched_count).then(|| end.to_string());

	(page, next_token, total)
}

/// Applies the substring, admin, locked, deactivated and user type filters to
/// one user and builds its `UserMinorDetails`, or returns `None` when the user
/// is filtered out.
async fn user_minor_details(
	services: crate::State,
	user_id: OwnedUserId,
	user_id_filter: Option<&str>,
	name_filter: Option<&str>,
	params: &ListParams<'_>,
) -> Option<UserMinorDetails> {
	if name_filter.is_none()
		&& let Some(needle) = user_id_filter
		&& !user_id.as_str().to_lowercase().contains(needle)
	{
		return None;
	}

	let displayname = services.profile.displayname(&user_id).await.ok();

	if let Some(needle) = name_filter {
		let in_localpart = user_id
			.localpart()
			.to_lowercase()
			.contains(needle);
		let in_displayname = displayname
			.as_deref()
			.is_some_and(|display| display.to_lowercase().contains(needle));

		if !in_localpart && !in_displayname {
			return None;
		}
	}

	let admin = services.admin.user_is_admin(&user_id).await;
	if let Some(want_admin) = params.admins
		&& want_admin != admin
	{
		return None;
	}

	let locked = services.users.is_locked(&user_id).await;
	if locked && !params.locked {
		return None;
	}

	let deactivated = services
		.users
		.is_deactivated(&user_id)
		.await
		.unwrap_or(false);

	let keep = match params.deactivated {
		| DeactivatedFilter::Any => true,
		| DeactivatedFilter::Only => deactivated,
		| DeactivatedFilter::Exclude => !deactivated,
	};

	if !keep {
		return None;
	}

	let user_type = user_type(services, &user_id).await;

	// As in Synapse, an empty string names the users that have no type.
	if params
		.not_user_type
		.iter()
		.any(|excluded| excluded == user_type.as_deref().unwrap_or_default())
	{
		return None;
	}

	let avatar_url = services
		.profile
		.avatar_url(&user_id)
		.map_ok(|url| url.to_string())
		.ok();

	let erased = services.users.is_erased(&user_id);

	let last_seen_ts = services
		.users
		.all_devices_metadata(&user_id)
		.ready_fold(None, |max, device| max.max(device.last_seen_ts));

	let (avatar_url, erased, last_seen_ts) = join!(avatar_url, erased, last_seen_ts);

	Some(UserMinorDetails {
		displayname,
		avatar_url,
		user_type,
		admin,
		deactivated,
		locked,
		erased,
		last_seen_ts,
		// tuwunel has no creation timestamp; emit a 0 sentinel (strict clients reject null).
		creation_ts: Some(MilliSecondsSinceUnixEpoch(UInt::from(0_u32))),
		..UserMinorDetails::new(user_id.into())
	})
}
