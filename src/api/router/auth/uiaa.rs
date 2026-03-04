use ruma::{
	CanonicalJsonValue, OwnedUserId,
	api::{
		IncomingRequest,
		client::uiaa::{AuthData, AuthFlow, AuthType, Jwt, UiaaInfo},
	},
};
use tuwunel_core::{Err, Error, Result, err, utils};
use tuwunel_service::{Services, uiaa::SESSION_ID_LENGTH};

use crate::{Ruma, client::jwt};

pub(crate) async fn auth_uiaa<T>(services: &Services, body: &Ruma<T>) -> Result<OwnedUserId>
where
	T: IncomingRequest + Send + Sync,
{
	let sender_device = body.sender_device()?;
	let sender_user = body.sender_user.as_deref();

	if let Some(sender_user) = sender_user {
		services
			.users
			.maybe_repair_legacy_sso_origin(sender_user)
			.await;
	}

	let sender_uses_sso = if let Some(sender_user) = sender_user {
		services
			.users
			.origin(sender_user)
			.await
			.is_ok_and(|origin| origin == "sso")
	} else {
		false
	};

	let flows = if sender_uses_sso {
		vec![AuthFlow::new([AuthType::Sso].into())]
	} else {
		vec![
			AuthFlow::new([AuthType::Password].into()),
			AuthFlow::new([AuthType::Jwt].into()),
		]
	};

	let mut uiaainfo = UiaaInfo {
		flows,
		..Default::default()
	};

	match body
		.json_body
		.as_ref()
		.and_then(CanonicalJsonValue::as_object)
		.and_then(|body| body.get("auth"))
		.cloned()
		.map(CanonicalJsonValue::into)
		.map(serde_json::from_value)
		.transpose()?
	{
		| Some(AuthData::Jwt(Jwt { ref token, .. })) => {
			if sender_uses_sso {
				return Err!(Request(Forbidden(
					"JWT UIAA is not allowed for SSO-origin users.",
				)));
			}

			let sender_user = jwt::validate_user(services, token)?;
			if !services.users.exists(&sender_user).await {
				return Err!(Request(NotFound("User {sender_user} is not registered.")));
			}

			// Success!
			Ok(sender_user)
		},
		| Some(ref auth) => {
			let sender_user = body
				.sender_user
				.as_deref()
				.ok_or_else(|| err!(Request(MissingToken("Missing access token."))))?;

			let (worked, uiaainfo) = services
				.uiaa
				.try_auth(sender_user, sender_device, auth, &uiaainfo)
				.await?;

			if !worked {
				return Err(Error::Uiaa(uiaainfo));
			}

			// Success!
			Ok(sender_user.to_owned())
		},
		| _ => match body.json_body {
			| Some(ref json) => {
				let sender_user = body
					.sender_user
					.as_deref()
					.ok_or_else(|| err!(Request(MissingToken("Missing access token."))))?;

				uiaainfo.session = Some(utils::random_string(SESSION_ID_LENGTH));
				services
					.uiaa
					.create(sender_user, sender_device, &uiaainfo, json);

				Err(Error::Uiaa(uiaainfo))
			},
			| _ => Err!(Request(NotJson("JSON body is not valid"))),
		},
	}
}
