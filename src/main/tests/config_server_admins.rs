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
	client::{Client, field, register},
	fixture::boot,
	timeline::append_message,
};

#[path = "client/admin_api.rs"]
mod admin_api;
mod appservice;
mod client;
mod fixture;
mod timeline;

#[test]
fn configured_admins_have_http_authority_but_not_room_membership() -> Result {
	boot(
		"config-server-admins",
		[
			"admin.room.create=true",
			"admin.escape_commands=true",
			"registration.enable=true",
			"registration.\
			 yes_i_am_very_very_sure_i_want_an_open_registration_server_prone_to_abuse=true",
			r#"server_admins=["@helper:localhost","@visitor:localhost","@reserved:localhost"]"#,
		],
		exercise,
	)
}

async fn exercise(services: &Services, base: &str) -> Result {
	let token = "config-admin-service-token-0123456789";
	let admin = &services.admin;
	let cache = &services.state_cache;
	// Provision the service accounts directly, not through public sign-up.
	let helper = register(services, "helper", token).await?;
	let visitor = register(services, "visitor", "config-admin-visitor-token-0123456789").await?;
	services.users.set_external(&visitor);

	let service = Client { services, base, token };
	let signup = Client { token: "", ..service };
	let response = signup
		.post("register", &signup_form("operator"))
		.await?;
	let human = Client {
		token: field(&response, "access_token")?,
		..service
	};
	let operator = user_id!("@operator:localhost");
	let ordinary =
		register(services, "ordinary", "config-admin-ordinary-token-0123456789").await?;
	let admin_room = admin.get_admin_room().await?;
	assert!(cache.is_joined(operator, &admin_room).await, "room bootstrap");
	assert!(admin.user_is_last_admin(operator).await);
	assert!(!admin.user_is_last_admin(&helper).await);
	assert!(!cache.is_joined(&helper, &admin_room).await);

	check_admin_updates(&human, &service, &helper).await?;
	let reserved = user_id!("@reserved:localhost");
	for (user, expected) in
		[(&*helper, true), (&*ordinary, false), (&*visitor, false), (reserved, false)]
	{
		let status = human
			.admin_api(Method::GET, &format!("v1/users/{user}/admin"), &json!({}), 200)
			.await?;
		assert_eq!(serde_json::from_str::<Value>(&status)?["admin"], expected, "{user}");
	}

	let room = service
		.create_room(&json!({ "preset": "private_chat" }))
		.await?;
	service
		.admin_api(Method::POST, &format!("v1/join/{room}"), &json!({ "user_id": ordinary }), 200)
		.await?;
	assert!(cache.is_joined(&ordinary, &room).await);
	let content = RoomMessageEventContent::text_plain("Hello.");
	let id = append_message(services, &helper, &room, &content).await?;
	let event = services.timeline.get_pdu(&id).await?;
	assert!(!admin.is_admin_command(&event, "\\!admin").await);

	admin.make_user_admin(&helper).await?;
	assert!(admin.is_admin_command(&event, "\\!admin").await);
	let revoke = human
		.admin_api(Method::PUT, &format!("v2/users/{helper}"), &json!({ "admin": false }), 400)
		.await?;
	assert_eq!(serde_json::from_str::<Value>(&revoke)?["errcode"], "M_INVALID_PARAM");
	assert!(revoke.contains("server_admins"), "{revoke}");
	assert!(cache.is_joined(&helper, &admin_room).await);

	check_reserved_signup(&signup, &helper).await
}

async fn check_admin_updates(
	human: &Client<'_>,
	service: &Client<'_>,
	helper: &UserId,
) -> Result {
	let services = human.services;
	let helper_path = format!("v2/users/{helper}");
	let reserved = user_id!("@reserved:localhost");
	let demote =
		json!({ "admin": false, "password": "different-password", "logout_devices": true });
	let password = services.users.password_hash(helper).await?;
	let device = services
		.users
		.find_from_token(service.token)
		.await?;

	human
		.admin_api(Method::PUT, &helper_path, &demote, 400)
		.await?;
	assert_eq!(services.users.password_hash(helper).await?, password);
	assert_eq!(
		services
			.users
			.find_from_token(service.token)
			.await?,
		device
	);
	human
		.admin_api(Method::PUT, &format!("v2/users/{reserved}"), &demote, 400)
		.await?;
	assert!(!services.users.exists(reserved).await);

	let body = human
		.admin_api(Method::PUT, &helper_path, &json!({ "admin": true }), 200)
		.await?;
	assert_eq!(serde_json::from_str::<Value>(&body)?["admin"], true);
	let room = services.admin.get_admin_room().await?;
	assert!(
		!services
			.state_cache
			.is_joined(helper, &room)
			.await
	);
	Ok(())
}

fn signup_form(username: &str) -> Value {
	json!({ "username": username, "password": "test-password", "auth": { "type": "m.login.dummy" } })
}

async fn check_reserved_signup(signup: &Client<'_>, helper: &UserId) -> Result {
	let url = signup.url("register");
	let reserved = user_id!("@reserved:localhost");
	let response: Value = signup
		.post_url(&url, &signup_form("reserved"))
		.await?
		.json()
		.await?;
	assert_eq!(response["errcode"], "M_USER_IN_USE");

	let bridge = Bridge {
		id: "example-bridge",
		token: "config-admin-bridge-token",
		sender_localpart: "bridge",
		users: "^@reserved:localhost$",
		aliases: None,
	};
	register_appservice(signup.services, &bridge).await?;
	let bridge = Client { token: bridge.token, ..*signup };
	let lookup = format!("v1/users/{helper}/admin?user_id={reserved}");
	bridge
		.admin_api(Method::GET, &lookup, &json!({}), 403)
		.await?;
	let form = json!({ "username": "reserved", "type": "m.login.application_service" });
	let response: Value = bridge.post_url(&url, &form).await?.json().await?;
	assert_eq!(response["errcode"], "M_USER_IN_USE");
	assert!(!signup.services.users.exists(reserved).await);
	Ok(())
}
