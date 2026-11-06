//! With `show_all_local_users_in_user_directory` off, the user directory
//! leaves out a deactivated local account that still shares a room.

#![cfg(test)]

mod support;

use serde_json::json;
use tuwunel_core::{
	Result,
	http::Method,
	ruma::{RoomId, user_id},
};
use tuwunel_service::users::DeactivationReason;

use self::support::{
	Harness,
	directory::{ACTIVE, ACTIVE_TOKEN, GONE, GONE_TOKEN, SEARCHER_TOKEN, request, search, setup},
};

/// With the option off, sharing a room makes an account visible, but not once
/// it is deactivated, even while its membership stays.
#[test]
fn shared_rooms_leave_out_deactivated_accounts() -> Result {
	let option = "show_all_local_users_in_user_directory=false".to_owned();
	let harness = Harness::new("mindroom_user_directory_deactivated_rooms", [option])?;

	harness.with_services(async |services| {
		let (router, _guard) = setup(&services).await?;

		assert!(search(&router).await.is_empty(), "nobody shares a room yet");

		let body = json!({"preset": "private_chat", "invite": [ACTIVE, GONE]});
		let room = request(&router, Method::POST, "/createRoom", SEARCHER_TOKEN, body).await;
		let room = room["room_id"]
			.as_str()
			.expect("createRoom returns a room ID");
		let join = format!("/rooms/{room}/join");

		for token in [ACTIVE_TOKEN, GONE_TOKEN] {
			request(&router, Method::POST, &join, token, json!({})).await;
		}

		assert_eq!(search(&router).await, [ACTIVE, GONE]);

		let gone = user_id!("@dir_gone:localhost");
		services
			.users
			.deactivate_account(gone, DeactivationReason::Admin)
			.await?;

		assert!(
			services
				.state_cache
				.is_joined(gone, <&RoomId>::try_from(room)?)
				.await,
			"the deactivated account should still be joined",
		);
		assert_eq!(search(&router).await, [ACTIVE]);

		Ok(())
	})
}
