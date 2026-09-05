use axum::extract::State;
use ruma::{
	DeviceId, UserId, api::client::keys::upload_keys, encryption::DeviceKeys, serde::Raw,
};
use tuwunel_core::{Err, Result, debug, err};
use tuwunel_service::{Services, users::DeviceKeysUpdate};

use crate::Ruma;

/// # `POST /_matrix/client/r0/keys/upload`
///
/// Publish end-to-end encryption keys for the sender device.
///
/// - Adds one time keys
/// - If there are no device keys yet: Adds device keys (TODO: merge with
///   existing keys?)
pub(crate) async fn upload_keys_route(
	State(services): State<crate::State>,
	body: Ruma<upload_keys::v3::Request>,
) -> Result<upload_keys::v3::Response> {
	let sender_user = body.sender_user();
	let sender_device = body.sender_device()?;

	services
		.users
		.add_one_time_keys(
			sender_user,
			sender_device,
			&body.one_time_keys,
			services.config.one_time_key_limit,
		)
		.await?;

	let fallback_keys = body
		.fallback_keys
		.iter()
		.map(|(id, val)| (id.as_ref(), val));

	services
		.users
		.add_fallback_keys(sender_user, sender_device, fallback_keys)
		.await?;

	if let Some(device_keys) = body.device_keys.as_ref() {
		store_device_keys(&services, sender_user, sender_device, device_keys).await?;
	}

	Ok(upload_keys::v3::Response {
		one_time_key_counts: services
			.users
			.count_one_time_keys(sender_user, sender_device)
			.await,
	})
}

async fn store_device_keys(
	services: &Services,
	sender_user: &UserId,
	sender_device: &DeviceId,
	device_keys: &Raw<DeviceKeys>,
) -> Result {
	let new_keys = device_keys.deserialize().map_err(|e| {
		err!(Request(BadJson(debug_warn!(
			?device_keys,
			"Invalid device keys JSON uploaded by client: {e}"
		))))
	})?;

	if new_keys.user_id != sender_user {
		return Err!(Request(Unknown(
			"User ID in keys uploaded does not match your own user ID"
		)));
	}
	if new_keys.device_id != sender_device {
		return Err!(Request(Unknown(
			"Device ID in keys uploaded does not match your own device ID"
		)));
	}

	match services
		.users
		.update_device_keys(sender_user, sender_device, device_keys, &new_keys)
		.await?
	{
		// Workaround for a nheko bug which omits cross-signing signatures when
		// re-uploading the same DeviceKeys: ignore an exact-copy re-upload so
		// the existing signatures are preserved.
		| DeviceKeysUpdate::Unchanged => {
			debug!(
				?sender_user,
				?sender_device,
				?device_keys,
				"Ignoring user uploaded keys as they are an exact copy already in the database"
			);

			Ok(())
		},

		// Identity keys for an existing device are immutable. Different key
		// material for the same device id means the client lost its crypto
		// store while keeping its access token; accepting the replacement
		// would silently break olm sessions and permanently poison the device
		// caches of every peer that saw the old identity. Force a fresh login
		// instead.
		| DeviceKeysUpdate::Conflict => Err!(Request(Forbidden(debug_warn!(
			?sender_user,
			?sender_device,
			"Rejecting upload of different identity keys for an existing device; device keys \
			 are immutable. Log out and log in again to register a new encryption identity."
		)))),

		| DeviceKeysUpdate::Inserted => Ok(()),
	}
}
