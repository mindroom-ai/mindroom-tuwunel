//! With `show_all_local_users_in_user_directory` on, the user directory
//! leaves out deactivated local accounts.

#![cfg(test)]

mod support;

use tuwunel_core::{
	Result,
	ruma::{UserId, user_id},
};
use tuwunel_service::users::{DeactivationReason, PASSWORD_SENTINEL};

use self::support::{
	Harness,
	directory::{ACTIVE, GONE, search, setup},
};

const RETURNING: &str = "@dir_returning:localhost";
const REMOTE: &str = "@dir_remote:remote.example.test";

/// With `show_all_local_users_in_user_directory` on, a deactivated account is
/// left out while active ones, an SSO account that came back after
/// deactivating itself, and a remote user (stored without a password, as a
/// deactivated account is) are listed.
#[test]
fn listing_all_local_users_leaves_out_deactivated_accounts() -> Result {
	let option = "show_all_local_users_in_user_directory=true".to_owned();
	let harness = Harness::new("mindroom_user_directory_deactivated_all", [option])?;

	harness.with_services(async |services| {
		let (router, _guard) = setup(&services).await?;
		let returning: &UserId = RETURNING.try_into()?;
		let remote: &UserId = REMOTE.try_into()?;

		services
			.users
			.create(returning, Some(PASSWORD_SENTINEL), Some("sso"))
			.await?;
		services.users.create(remote, None, None).await?;

		let everyone = [ACTIVE, GONE, REMOTE, RETURNING];
		assert_eq!(search(&router).await, everyone);

		services
			.deactivate
			.full_deactivate(user_id!("@dir_gone:localhost"), false, DeactivationReason::Admin)
			.await?;
		services
			.deactivate
			.full_deactivate(returning, false, DeactivationReason::SelfService)
			.await?;

		assert_eq!(search(&router).await, [ACTIVE, REMOTE]);

		let reactivated = services
			.users
			.maybe_reactivate_deactivated_sso(returning)
			.await?;
		assert!(reactivated, "the self-deactivated SSO account should reactivate");
		assert_eq!(search(&router).await, [ACTIVE, REMOTE, RETURNING]);

		Ok(())
	})
}
