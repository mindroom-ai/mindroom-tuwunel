use axum::extract::State;
use ruma::api::client::dehydrated_device::put_dehydrated_device::unstable::{Request, Response};
use tuwunel_core::{Err, Result};
use tuwunel_service::sending::MAX_EDU_CONTENT_BYTES;

use crate::{ClientIp, Ruma};

/// # `PUT /_matrix/client/../dehydrated_device`
///
/// Creates or overwrites the user's dehydrated device.
///
/// Any existing dehydrated device is removed first, then the new one is
/// created with the keys the request uploads.
#[tracing::instrument(
	level = "debug",
	skip_all,
	fields(
		%client,
	),
)]
pub(crate) async fn put_dehydrated_device_route(
	State(services): State<crate::State>,
	ClientIp(client): ClientIp,
	body: Ruma<Request>,
) -> Result<Response> {
	let sender_user = body
		.sender_user
		.as_deref()
		.expect("AccessToken authentication required");

	// Other servers receive the keys in device list updates.
	if body.body.device_keys.json().get().len() > MAX_EDU_CONTENT_BYTES {
		return Err!(Request(TooLarge("Device keys are too large.")));
	}

	let device_id = body.body.device_id.clone();

	services
		.users
		.set_dehydrated_device(sender_user, body.body)
		.await?;

	Ok(Response { device_id })
}
