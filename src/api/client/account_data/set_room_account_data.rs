use axum::extract::State;
use ruma::api::client::config::set_room_account_data;
use tuwunel_core::Result;

use super::{require_owner, set_account_data};
use crate::Ruma;

/// # `PUT /_matrix/client/r0/user/{userId}/rooms/{roomId}/account_data/{type}`
///
/// Sets some room account data for the sender user.
pub(crate) async fn set_room_account_data_route(
	State(services): State<crate::State>,
	body: Ruma<set_room_account_data::v3::Request>,
) -> Result<set_room_account_data::v3::Response> {
	require_owner(&services, &body, &body.user_id).await?;

	set_account_data(
		&services,
		Some(&body.room_id),
		&body.user_id,
		&body.event_type.to_string(),
		body.data.json(),
	)
	.await?;

	Ok(set_room_account_data::v3::Response {})
}
