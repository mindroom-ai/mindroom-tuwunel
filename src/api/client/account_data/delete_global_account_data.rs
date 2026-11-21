use axum::extract::State;
use ruma::api::client::config::delete_global_account_data;
use tuwunel_core::Result;

use super::{require_owner, validate_event_type};
use crate::Ruma;

/// # `DELETE /_matrix/client/unstable/org.matrix.msc3391/user/{userId}/account_data/{type}`
///
/// MSC3391: erase the named global account data type for the user.
pub(crate) async fn delete_global_account_data_route(
	State(services): State<crate::State>,
	body: Ruma<delete_global_account_data::unstable::Request>,
) -> Result<delete_global_account_data::unstable::Response> {
	require_owner(&services, &body, &body.user_id).await?;

	validate_event_type(&body.event_type.to_cow_str())?;

	services
		.account_data
		.delete(None, &body.user_id, body.event_type.to_string().into())
		.await?;

	Ok(delete_global_account_data::unstable::Response {})
}
