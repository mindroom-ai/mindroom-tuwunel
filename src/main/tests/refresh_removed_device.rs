#![cfg(test)]

use tuwunel_core::{
	Err, Result,
	ruma::{UserId, device_id},
};
use tuwunel_service::{Services, users::device::generate_refresh_token};

use self::fixture::boot;

#[expect(
	dead_code,
	reason = "Only listener readiness is shared with the client API harness."
)]
mod client;

mod fixture;

/// A removed device is issued no new tokens.
///
/// A refresh still in flight when its device is removed must not leave tokens
/// behind that point at the removed device.
#[test]
fn removed_device_is_issued_no_tokens() -> Result {
	let options = ["argon2_m_cost=64", "argon2_t_cost=1"];

	boot("refresh-removed-device", options, exercise)
}

async fn exercise(services: &Services, _base: &str) -> Result {
	let user = UserId::parse_with_server_name("removed", services.globals.server_name())?;
	let device = device_id!("REMOVEDDEVICE");

	services
		.users
		.create(&user, Some("password"), None)
		.await?;

	let (access, expires_in) = services.users.generate_access_token(true);
	let refresh = generate_refresh_token();

	services
		.users
		.create_device(
			&user,
			Some(device),
			(Some(&access), expires_in),
			Some(&refresh),
			None,
			None,
		)
		.await?;

	services.users.remove_device(&user, device).await;

	let (new_access, new_expires_in) = services.users.generate_access_token(true);
	let new_refresh = generate_refresh_token();

	let issued = services
		.users
		.set_access_token(&user, device, &new_access, new_expires_in, Some(&new_refresh))
		.await;

	if issued.is_ok() {
		return Err!("tokens were issued to a removed device");
	}

	for token in [&new_access, &new_refresh] {
		if services
			.users
			.find_from_token(token)
			.await
			.is_ok()
		{
			return Err!("a token issued to a removed device resolves");
		}
	}

	Ok(())
}
