use std::{
	collections::BTreeMap,
	sync::{Arc, RwLock},
};

use ruma::{
	CanonicalJsonValue, DeviceId, OwnedDeviceId, OwnedUserId, UserId,
	api::client::{
		error::{ErrorKind, StandardErrorBody},
		uiaa::{AuthData, AuthType, Password, UiaaInfo, UserIdentifier},
	},
};
use tuwunel_core::{
	Err, Result, debug_warn, err, error, extract, implement,
	utils::{self, BoolExt, hash, string::EMPTY},
};
use tuwunel_database::{Deserialized, Json, Map};

use crate::users::PASSWORD_SENTINEL;

pub struct Service {
	userdevicesessionid_uiaarequest: RwLock<RequestMap>,
	db: Data,
	services: Arc<crate::services::OnceServices>,
}

struct Data {
	sessionid_userdeviceid: Arc<Map>,
	userdevicesessionid_uiaainfo: Arc<Map>,
}

type RequestMap = BTreeMap<RequestKey, CanonicalJsonValue>;
type RequestKey = (OwnedUserId, OwnedDeviceId, String);

pub const SESSION_ID_LENGTH: usize = 32;

impl crate::Service for Service {
	fn build(args: &crate::Args<'_>) -> Result<Arc<Self>> {
		Ok(Arc::new(Self {
			userdevicesessionid_uiaarequest: RwLock::new(RequestMap::new()),
			db: Data {
				sessionid_userdeviceid: args.db["sessionid_userdeviceid"].clone(),
				userdevicesessionid_uiaainfo: args.db["userdevicesessionid_uiaainfo"].clone(),
			},
			services: args.services.clone(),
		}))
	}

	fn name(&self) -> &str { crate::service::make_name(std::module_path!()) }
}

/// Creates a new Uiaa session. Make sure the session token is unique.
#[implement(Service)]
pub fn create(
	&self,
	user_id: &UserId,
	device_id: &DeviceId,
	uiaainfo: &UiaaInfo,
	json_body: &CanonicalJsonValue,
) {
	// TODO: better session error handling (why is uiaainfo.session optional in
	// ruma?)
	let session = uiaainfo
		.session
		.as_ref()
		.expect("session should be set");

	self.set_uiaa_request(user_id, device_id, session, json_body);

	self.update_uiaa_session(user_id, device_id, session, Some(uiaainfo));
}

#[implement(Service)]
#[allow(clippy::useless_let_if_seq)]
pub async fn try_auth(
	&self,
	user_id: &UserId,
	device_id: &DeviceId,
	auth: &AuthData,
	uiaainfo: &UiaaInfo,
) -> Result<(bool, UiaaInfo)> {
	let mut uiaainfo = if let Some(session) = auth.session() {
		self.get_uiaa_session(user_id, device_id, session)
			.await?
	} else {
		uiaainfo.clone()
	};

	if uiaainfo.session.is_none() {
		uiaainfo.session = Some(utils::random_string(SESSION_ID_LENGTH));
	}

	match auth {
		// Find out what the user completed
		| AuthData::Password(Password { identifier, password, user, .. }) => {
			let username = extract!(identifier, x in Some(UserIdentifier::UserIdOrLocalpart(x)))
				.or_else(|| cfg!(feature = "element_hacks").and(user.as_ref()))
				.ok_or(err!(Request(Unrecognized("Identifier type not recognized."))))?;

			let user_id_from_username = UserId::parse_with_server_name(
				username.clone(),
				self.services.globals.server_name(),
			)
			.map_err(|_| err!(Request(InvalidParam("User ID is invalid."))))?;

			// Check if the access token being used matches the credentials used for UIAA
			if user_id.localpart() != user_id_from_username.localpart() {
				return Err!(Request(Forbidden("User ID and access token mismatch.")));
			}

			// Check if password is correct
			let user_id = user_id_from_username;
			let mut password_verified = false;
			let mut password_sentinel = false;

			// First try local password hash verification
			if let Ok(hash) = self.services.users.password_hash(&user_id).await {
				password_sentinel = hash == PASSWORD_SENTINEL;
				password_verified = hash::verify_password(password, &hash).is_ok();
			}

			// If local password verification failed, try LDAP authentication
			#[cfg(feature = "ldap")]
			if !password_verified && self.services.server.config.ldap.enable {
				// Search for user in LDAP to get their DN
				if let Ok(dns) = self.services.users.search_ldap(&user_id).await
					&& let Some((user_dn, _is_admin)) = dns.first()
				{
					// Try to authenticate with LDAP
					password_verified = self
						.services
						.users
						.auth_ldap(user_dn, password)
						.await
						.is_ok();
				}
			}

			// For SSO users that have never set a password, allow.
			if !password_verified
				&& password_sentinel
				&& self
					.services
					.oauth
					.sessions
					.exists_for_user(&user_id)
					.await
			{
				return Ok((true, uiaainfo));
			}

			if !password_verified {
				uiaainfo.auth_error = Some(StandardErrorBody {
					kind: ErrorKind::forbidden(),
					message: "Invalid username or password.".to_owned(),
				});

				return Ok((false, uiaainfo));
			}

			// Password was correct! Let's add it to `completed`
			uiaainfo.completed.push(AuthType::Password);
		},
		| AuthData::RegistrationToken(t) => {
			let token = t.token.trim();
			if self
				.services
				.registration_tokens
				.try_consume(token)
				.await
				.is_ok()
			{
				uiaainfo
					.completed
					.push(AuthType::RegistrationToken);
			} else {
				uiaainfo.auth_error = Some(StandardErrorBody {
					kind: ErrorKind::forbidden(),
					message: "Invalid registration token.".to_owned(),
				});

				return Ok((false, uiaainfo));
			}
		},
		| AuthData::FallbackAcknowledgement(session) => {
			debug_warn!("FallbackAcknowledgement: {session:?}");
		},
		| AuthData::Dummy(_) => {
			uiaainfo.completed.push(AuthType::Dummy);
		},
		| auth => error!("AuthData type not supported: {auth:?}"),
	}

	// Check if a flow now succeeds
	let mut completed = false;
	'flows: for flow in &mut uiaainfo.flows {
		for stage in &flow.stages {
			if !uiaainfo.completed.contains(stage) {
				continue 'flows;
			}
		}
		// We didn't break, so this flow succeeded!
		completed = true;
	}

	let session = uiaainfo
		.session
		.as_ref()
		.expect("session is always set");

	if !completed {
		self.update_uiaa_session(user_id, device_id, session, Some(&uiaainfo));

		return Ok((false, uiaainfo));
	}

	// UIAA was successful! Remove this session and return true
	self.update_uiaa_session(user_id, device_id, session, None);

	Ok((true, uiaainfo))
}

#[implement(Service)]
fn set_uiaa_request(
	&self,
	user_id: &UserId,
	device_id: &DeviceId,
	session: &str,
	request: &CanonicalJsonValue,
) {
	let key = (user_id.to_owned(), device_id.to_owned(), session.to_owned());

	self.userdevicesessionid_uiaarequest
		.write()
		.expect("locked for writing")
		.insert(key, request.to_owned());
}

#[implement(Service)]
pub fn get_uiaa_request(
	&self,
	user_id: &UserId,
	device_id: Option<&DeviceId>,
	session: &str,
) -> Option<CanonicalJsonValue> {
	let device_id = device_id.unwrap_or_else(|| EMPTY.into());
	let key = (user_id.to_owned(), device_id.to_owned(), session.to_owned());

	self.userdevicesessionid_uiaarequest
		.read()
		.expect("locked for reading")
		.get(&key)
		.cloned()
}

#[implement(Service)]
pub async fn complete_stage(
	&self,
	user_id: &UserId,
	session: &str,
	stage: AuthType,
) -> Result {
	let (session_user, session_device): (OwnedUserId, OwnedDeviceId) = self
		.db
		.sessionid_userdeviceid
		.get(session)
		.await
		.deserialized()
		.map_err(|_| err!(Request(Forbidden("UIAA session does not exist."))))?;

	if session_user.as_str() != user_id.as_str() {
		return Err!(Request(Forbidden("UIAA session and user mismatch.")));
	}

	let mut uiaainfo = self
		.get_uiaa_session(&session_user, &session_device, session)
		.await?;

	if !uiaainfo.completed.contains(&stage) {
		uiaainfo.completed.push(stage);
	}

	self.update_uiaa_session(&session_user, &session_device, session, Some(&uiaainfo));
	Ok(())
}

#[implement(Service)]
fn update_uiaa_session(
	&self,
	user_id: &UserId,
	device_id: &DeviceId,
	session: &str,
	uiaainfo: Option<&UiaaInfo>,
) {
	let key = (user_id, device_id, session);

	if let Some(uiaainfo) = uiaainfo {
		self.db
			.userdevicesessionid_uiaainfo
			.put(key, Json(uiaainfo));
		self
			.db
			.sessionid_userdeviceid
			.raw_put(session, (user_id, device_id));
	} else {
		self.db.userdevicesessionid_uiaainfo.del(key);
		self.db.sessionid_userdeviceid.remove(session);
	}
}

#[implement(Service)]
async fn get_uiaa_session(
	&self,
	user_id: &UserId,
	device_id: &DeviceId,
	session: &str,
) -> Result<UiaaInfo> {
	let key = (user_id, device_id, session);

	self.db
		.userdevicesessionid_uiaainfo
		.qry(&key)
		.await
		.deserialized()
		.map_err(|_| err!(Request(Forbidden("UIAA session does not exist."))))
}

#[cfg(test)]
mod tests {
	use std::{
		fs,
		path::{Path, PathBuf},
		sync::{
			Arc, RwLock,
			atomic::{AtomicU64, Ordering},
		},
	};

	use ruma::{
		CanonicalJsonValue, device_id, user_id,
		api::client::uiaa::{
			AuthData, AuthFlow, AuthType, FallbackAcknowledgement, UiaaInfo,
		},
	};
	use tracing::subscriber::NoSubscriber;
	use tuwunel_core::{
		Server,
		config::Config,
		log::{Logging, capture::State as CaptureState},
	};
	use tuwunel_database::{Database, Deserialized};

	use super::{Data, RequestMap, Service};

	static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(1);

	#[tokio::test]
	async fn complete_stage_persists_across_restart() {
		let temp_dir = unique_temp_dir();
		let user_id = user_id!("@alice:example.com");
		let device_id = device_id!("DEVICE_A");
		let session = "uiaa-restart-session";

		{
			let (_server, db) = open_server_db(&temp_dir).await;
			let service = make_service(&db);
			let uiaainfo = sso_uiaa_info(session);
			service.create(
				user_id,
				device_id,
				&uiaainfo,
				&CanonicalJsonValue::Object(Default::default()),
			);
		}

		let (_server, db) = open_server_db(&temp_dir).await;
		let service = make_service(&db);

		service
			.complete_stage(user_id, session, AuthType::Sso)
			.await
			.expect("stage completion should work after restart");

		let (worked, _) = service
			.try_auth(
				user_id,
				device_id,
				&AuthData::FallbackAcknowledgement(FallbackAcknowledgement {
					session: session.to_owned(),
				}),
				&UiaaInfo::default(),
			)
			.await
			.expect("fallback acknowledgement should complete session");
		assert!(worked, "expected fallback acknowledgement to finish UIAA");

		assert!(
			service.db.sessionid_userdeviceid.get(session).await.is_err(),
			"reverse UIAA lookup should be removed after completion"
		);
		assert!(
			service
				.db
				.userdevicesessionid_uiaainfo
				.qry(&(user_id, device_id, session))
				.await
				.is_err(),
			"forward UIAA session entry should be removed after completion"
		);

		cleanup_temp_dir(&temp_dir);
	}

	#[tokio::test]
	async fn complete_stage_rejects_user_mismatch() {
		let temp_dir = unique_temp_dir();
		let (_server, db) = open_server_db(&temp_dir).await;
		let service = make_service(&db);

		let session_owner = user_id!("@alice:example.com");
		let other_user = user_id!("@bob:example.com");
		let device_id = device_id!("DEVICE_MISMATCH");
		let session = "uiaa-mismatch-session";

		service.create(
			session_owner,
			device_id,
			&sso_uiaa_info(session),
			&CanonicalJsonValue::Object(Default::default()),
		);

		service
			.complete_stage(other_user, session, AuthType::Sso)
			.await
			.expect_err("mismatched user must not complete another user's UIAA session");

		let (stored_user, stored_device): (ruma::OwnedUserId, ruma::OwnedDeviceId) = service
			.db
			.sessionid_userdeviceid
			.get(session)
			.await
			.deserialized()
			.expect("reverse entry should remain after failed completion");
		assert_eq!(stored_user, session_owner.to_owned(), "session owner must be unchanged");
		assert_eq!(stored_device, device_id.to_owned(), "session device must be unchanged");

		assert!(
			service
				.db
				.userdevicesessionid_uiaainfo
				.qry(&(session_owner, device_id, session))
				.await
				.is_ok(),
			"forward UIAA session entry should remain after failed completion"
		);

		cleanup_temp_dir(&temp_dir);
	}

	#[tokio::test]
	async fn fallback_acknowledgement_removes_forward_and_reverse_entries() {
		let temp_dir = unique_temp_dir();
		let (_server, db) = open_server_db(&temp_dir).await;
		let service = make_service(&db);

		let user_id = user_id!("@carol:example.com");
		let device_id = device_id!("DEVICE_CLEANUP");
		let session = "uiaa-cleanup-session";

		service.create(
			user_id,
			device_id,
			&sso_uiaa_info(session),
			&CanonicalJsonValue::Object(Default::default()),
		);

		service
			.complete_stage(user_id, session, AuthType::Sso)
			.await
			.expect("SSO stage should complete");

		let (worked, _) = service
			.try_auth(
				user_id,
				device_id,
				&AuthData::FallbackAcknowledgement(FallbackAcknowledgement {
					session: session.to_owned(),
				}),
				&UiaaInfo::default(),
			)
			.await
			.expect("fallback acknowledgement should succeed");
		assert!(worked, "fallback acknowledgement should complete the flow");

		assert!(
			service.db.sessionid_userdeviceid.get(session).await.is_err(),
			"reverse UIAA lookup must be deleted after completion"
		);
		assert!(
			service
				.db
				.userdevicesessionid_uiaainfo
				.qry(&(user_id, device_id, session))
				.await
				.is_err(),
			"forward UIAA session must be deleted after completion"
		);

		cleanup_temp_dir(&temp_dir);
	}

	fn make_service(db: &Arc<Database>) -> Arc<Service> {
		Arc::new(Service {
			userdevicesessionid_uiaarequest: RwLock::new(RequestMap::new()),
			db: Data {
				sessionid_userdeviceid: db["sessionid_userdeviceid"].clone(),
				userdevicesessionid_uiaainfo: db["userdevicesessionid_uiaainfo"].clone(),
			},
			services: Arc::new(crate::services::OnceServices::default()),
		})
	}

	async fn open_server_db(temp_dir: &Path) -> (Arc<Server>, Arc<Database>) {
		let db_path = temp_dir.join("db");
		let config_path = temp_dir.join("tuwunel.toml");

		fs::create_dir_all(temp_dir).expect("create test temp dir");
		let config_contents = format!(
			r#"[global]
server_name = "example.com"
database_path = "{}"
"#,
			db_path.display(),
		);
		fs::write(&config_path, config_contents).expect("write test config");

		let figment = Config::load(std::iter::once(config_path.as_path())).expect("load config");
		let config = Config::new(&figment).expect("parse config");
		let log = Logging {
			reload: Default::default(),
			capture: Arc::new(CaptureState::new()),
			subscriber: Arc::new(NoSubscriber::new()),
		};
		let server = Arc::new(Server::new(config, Some(tokio::runtime::Handle::current()), log));
		let db = Database::open(&server).await.expect("open test database");

		(server, db)
	}

	fn sso_uiaa_info(session: &str) -> UiaaInfo {
		UiaaInfo {
			flows: vec![AuthFlow::new([AuthType::Sso].into())],
			session: Some(session.to_owned()),
			..Default::default()
		}
	}

	fn unique_temp_dir() -> PathBuf {
		let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
		let pid = std::process::id();
		let path = std::env::temp_dir().join(format!("tuwunel-uiaa-{pid}-{id}"));
		fs::create_dir_all(&path).expect("create unique test dir");
		path
	}

	fn cleanup_temp_dir(path: &Path) {
		let _ = fs::remove_dir_all(path);
	}
}
