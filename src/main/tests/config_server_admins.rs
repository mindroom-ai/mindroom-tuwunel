#![cfg(test)]

use reqwest::Method;
use serde_json::{Value, json};
use tuwunel_core::{
	Result,
	ruma::{UserId, events::room::message::RoomMessageEventContent, user_id},
};
use tuwunel_service::Services;

use self::{
	appservice::{Bridge, register_appservice},
	client::{Client, register},
	fixture::boot,
	timeline::append_message,
};

mod appservice;
#[expect(dead_code, reason = "Only the registration and HTTP helpers are used.")]
mod client;
mod fixture;
mod timeline;

const SERVICE_TOKEN: &str = "config-admin-service-token-0123456789";

#[test]
fn configured_admins_have_http_authority_but_not_room_membership() -> Result {
	boot(
		"config-server-admins",
		[
			"admin.room.create=true",
			"admin.escape_commands=true",
			"registration.enable=true",
			"registration.yes_i_am_very_very_sure_i_want_an_open_registration_server_prone_to_abuse=true",
			r#"server_admins=["@helper:localhost","@visitor:localhost","@reserved:localhost","@reserved_bridge:localhost"]"#,
		],
		exercise,
	)
}

async fn exercise(services: &Services, base: &str) -> Result {
	let helper = user_id!("@helper:localhost");
	let visitor = user_id!("@visitor:localhost");
	let reserved = user_id!("@reserved:localhost");
	// Provision the service accounts directly, not through public sign-up.
	for user in [helper, visitor] {
		services.users.create_new(user, Some("test-password"), None).await?;
	}
	services.users.set_external(visitor);
	services.users.create_device(helper, None, (Some(SERVICE_TOKEN), None), None, None, None).await?;
	let service = Client { services, base, token: SERVICE_TOKEN };
	let signup = Client { token: "", ..service };
	let operator = user_id!("@operator:localhost");
	let response = signup.post("register", &json!({
		"username": "operator", "password": "test-password", "auth": { "type": "m.login.dummy" },
	})).await?;
	let human = Client { token: response["access_token"].as_str().expect("registration token"), ..service };
	let ordinary = register(services, "ordinary", "config-admin-ordinary-token-0123456789").await?;
	let admin_room = services.admin.get_admin_room().await?;
	assert!(services.state_cache.is_joined(operator, &admin_room).await, "room bootstrap");
	assert!(services.admin.user_is_last_admin(operator).await);
	assert!(!services.admin.user_is_last_admin(helper).await);
	assert!(!services.state_cache.is_joined(helper, &admin_room).await);

	let status = async |user: &UserId| -> Result<Value> {
		let url = format!("{base}/_synapse/admin/v1/users/{user}/admin");
		Ok(human.request(Method::GET, &url, &json!({})).await?.error_for_status()?.json().await?)
	};
	assert_eq!(status(&ordinary).await?["admin"], false);
	assert_eq!(status(visitor).await?["admin"], false);
	let helper_admin = status(helper).await?["admin"] == true;

	let room = service.create_room(&json!({ "preset": "private_chat" })).await?;
	let joined = service.post_url(
		&format!("{base}/_synapse/admin/v1/join/{room}"),
		&json!({ "user_id": ordinary }),
	).await?.status().is_success() && services.state_cache.is_joined(&ordinary, &room).await;
	let id = append_message(services, helper, &room, &RoomMessageEventContent::text_plain("Hello.")).await?;
	let event = services.timeline.get_pdu(&id).await?;
	assert!(!services.admin.is_admin_command(&event, "!admin").await);
	assert!(!services.admin.is_admin_command(&event, "\\!admin").await);

	let refused = signup.post_url(&signup.url("register"), &json!({
		"username": "reserved", "password": "test-password", "auth": { "type": "m.login.dummy" },
	})).await?.json::<Value>().await?["errcode"] == "M_USER_IN_USE";

	let bridge = Bridge { id: "example-bridge", token: "config-admin-bridge-token", sender_localpart: "bridge", users: "^@reserved_bridge:localhost$", aliases: None };
	register_appservice(services, &bridge).await?;
	let bridge = Client { token: bridge.token, ..service };
	let impersonation = format!("{base}/_synapse/admin/v1/users/{helper}/admin?user_id=@reserved_bridge:localhost");
	assert_eq!(bridge.request(Method::GET, &impersonation, &json!({})).await?.status().as_u16(), 403);
	let bridge_refused = bridge.post_url(&bridge.url("register"), &json!({
		"username": "reserved_bridge", "type": "m.login.application_service",
	})).await?.json::<Value>().await?["errcode"] == "M_USER_IN_USE";

	services.admin.make_user_admin(helper).await?;
	assert!(services.admin.is_admin_command(&event, "\\!admin").await);
	let revoke = human.request(Method::PUT, &format!("{base}/_synapse/admin/v2/users/{helper}"),
		&json!({ "admin": false }),
	).await?.json::<Value>().await?;
	let membership_kept = services.state_cache.is_joined(helper, &admin_room).await;

	assert_eq!([helper_admin, joined, refused, bridge_refused, revoke["errcode"] == "M_INVALID_PARAM", membership_kept], [true; 6],
		"expected config grant, admin join, signup refusal, demotion refusal ({revoke}), and intact membership");
	assert!(!services.users.exists(reserved).await);
	Ok(())
}
