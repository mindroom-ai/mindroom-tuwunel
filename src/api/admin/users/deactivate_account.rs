use axum::extract::State;
use synapse_admin_api::users::deactivate_account::v1 as deactivate_account;
use tuwunel_core::{Err, Result};
use tuwunel_service::users::{DeactivationGuard, DeactivationReason};

use crate::{Ruma, refuse_server_user, require_admin};

/// # `POST /_synapse/admin/v1/deactivate/{user_id}`
///
/// Refuses another active server admin and the admin who asks, as locking and
/// suspending do: a deactivation leaves every room, the admins room too, and
/// clears the profile, and reactivating restores neither. The deactivation
/// itself looks for the admin, under the admins room's lock, so an account
/// promoted while the request ran is refused as well.
pub(crate) async fn admin_deactivate_account_route(
	State(services): State<crate::State>,
	body: Ruma<deactivate_account::Request>,
) -> Result<deactivate_account::Response> {
	let sender_user = body.sender_user();

	require_admin(&services, sender_user).await?;

	if !services.globals.user_is_local(&body.user_id) {
		return Err!(Request(InvalidParam("Can only deactivate local users")));
	}

	refuse_server_user(services, &body.user_id)?;

	if !services.users.exists(&body.user_id).await {
		return Err!(Request(NotFound("User not found")));
	}

	if sender_user == body.user_id {
		return Err!(Request(Forbidden("You cannot deactivate your own account here")));
	}

	services
		.deactivate
		.full_deactivate_guarded(
			&body.user_id,
			body.erase,
			DeactivationReason::Admin,
			DeactivationGuard::Admins,
		)
		.await?;

	Ok(deactivate_account::Response::new("success".to_owned()))
}
