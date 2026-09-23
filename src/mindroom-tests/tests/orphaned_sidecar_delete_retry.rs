//! A storage failure must leave sidecars indexed so a later sweep can retry.

mod support;

#[cfg(test)]
mod tests {
	use std::{
		collections::HashMap,
		sync::{Arc, Mutex},
		time::Duration,
	};

	use axum::{
		Router,
		body::Bytes,
		extract::State,
		response::{IntoResponse, Response},
	};
	use tuwunel_core::{
		Result,
		http::{Method, StatusCode, Uri, header},
		ruma::{Mxc, user_id},
		utils::content_disposition::make_content_disposition,
	};
	use tuwunel_service::{
		Services,
		edit_purge::{OrphanedSidecarSweep, UploaderFilter},
		media::Dim,
	};

	use super::support::Harness;

	const CONTENT: &[u8] = br#"{"body":"retry this sidecar"}"#;

	#[derive(Default)]
	struct Storage {
		objects: Mutex<HashMap<String, Vec<u8>>>,
		deny_delete: Mutex<Option<String>>,
	}

	/// Only the external S3 boundary is faked; indexing, ownership, candidate
	/// selection, local storage, and deletion use the production services.
	#[test]
	fn orphaned_sidecar_delete_failure_can_be_retried() -> Result {
		let mut harness = Harness::new("mindroom_sidecar_delete_retry", [])?;
		let storage = Arc::new(Storage::default());
		let mock = harness.mock_server(
			Router::new()
				.fallback(storage_request)
				.with_state(storage.clone()),
		)?;
		harness.args.option.extend([
			"media_storage_providers=[\"media\",\"retry\"]".to_owned(),
			"store_media_on_providers=[\"media\",\"retry\"]".to_owned(),
			"storage_provider.retry.s3.bucket=\"sidecars\"".to_owned(),
			"storage_provider.retry.s3.region=\"us-east-1\"".to_owned(),
			format!("storage_provider.retry.s3.endpoint=\"{}\"", mock.base_url),
			"storage_provider.retry.s3.use_https=false".to_owned(),
			"storage_provider.retry.s3.use_signatures=false".to_owned(),
			"storage_provider.retry.s3.startup_check=false".to_owned(),
		]);

		harness.with_services(async |services| {
			let user = user_id!("@sidecar_bot:localhost");
			let mxc = Mxc {
				server_name: services.globals.server_name(),
				media_id: "retrySidecar",
			};
			let disposition = make_content_disposition(
				None,
				Some("application/json"),
				Some("message-content.json"),
			);
			services
				.media
				.create(&mxc, Some(user), Some(&disposition), Some("application/json"), CONTENT)
				.await?;

			// The local replica is deleted first. The remote replica still exists
			// and must keep its indexes even though another provider succeeded.
			let local = std::fs::read_dir(services.media.get_media_dir())?
				.next()
				.expect("stored sidecar")?
				.path();
			std::fs::File::options()
				.write(true)
				.open(&local)?
				.set_modified(std::time::SystemTime::now() - Duration::from_hours(72))?;
			*storage.deny_delete.lock().expect("storage lock") = Some(
				local
					.file_name()
					.expect("object name")
					.to_string_lossy()
					.into_owned(),
			);
			let sweep = OrphanedSidecarSweep {
				older_than: Duration::from_hours(48),
				uploader: UploaderFilter::Any,
				limit: 100,
				execute: true,
			};
			let report = services
				.edit_purge
				.sweep_orphaned_long_text_sidecars(&sweep)
				.await?;
			assert_eq!(report.deleted, 0, "failed storage deletion is not reported as success");
			assert_eq!(report.failed, 1);
			assert_eq!(report.deleted_bytes, 0);
			assert_eq!(report.failures.len(), 1);
			assert!(!local.exists(), "the first provider already deleted its replica");
			assert!(services.media.get_metadata(&mxc).await.is_some());
			assert!(
				services
					.media
					.mxc_is_owned_by_user(&mxc, user)
					.await
			);
			assert_eq!(
				storage
					.objects
					.lock()
					.expect("storage lock")
					.len(),
				1
			);

			*storage.deny_delete.lock().expect("storage lock") = None;
			let report = services
				.edit_purge
				.sweep_orphaned_long_text_sidecars(&sweep)
				.await?;
			assert_eq!(report.deleted, 1, "a later sweep retries the retained upload");
			assert_eq!(report.failed, 0);
			assert_eq!(report.deleted_bytes, u64::try_from(CONTENT.len()).expect("content size"));
			assert!(services.media.get_metadata(&mxc).await.is_none());
			assert!(
				!services
					.media
					.mxc_is_owned_by_user(&mxc, user)
					.await
			);
			assert!(
				storage
					.objects
					.lock()
					.expect("storage lock")
					.is_empty()
			);
			thumbnail_failure_keeps_indexes(&services, &storage).await
		})
	}

	async fn thumbnail_failure_keeps_indexes(services: &Services, storage: &Storage) -> Result {
		let user = user_id!("@sidecar_bot:localhost");
		let mxc = Mxc {
			server_name: services.globals.server_name(),
			media_id: "retryThumbnail",
		};
		services
			.media
			.create(&mxc, Some(user), None, Some("image/png"), CONTENT)
			.await?;
		let original = storage
			.objects
			.lock()
			.expect("storage lock")
			.keys()
			.next()
			.expect("stored original")
			.clone();
		services
			.media
			.upload_thumbnail(
				&mxc,
				None,
				Some("image/png"),
				&Dim::new(32, 32, None),
				b"thumbnail",
			)
			.await?;
		let thumbnail = storage
			.objects
			.lock()
			.expect("storage lock")
			.keys()
			.find(|key| **key != original)
			.expect("stored thumbnail")
			.clone();
		*storage.deny_delete.lock().expect("storage lock") = Some(thumbnail.clone());

		services
			.media
			.delete_owned_by(&mxc, user)
			.await
			.expect_err("thumbnail deletion failure must reach the caller");
		assert!(
			services.media.get_metadata(&mxc).await.is_some(),
			"keep all metadata on failure"
		);
		assert!(
			services
				.media
				.mxc_is_owned_by_user(&mxc, user)
				.await,
			"failed thumbnail deletion must retain ownership"
		);
		let (original_deleted, thumbnail_stored) = {
			let objects = storage.objects.lock().expect("storage lock");
			(!objects.contains_key(&original), objects.contains_key(&thumbnail))
		};
		assert!(original_deleted, "original was deleted before thumbnail failed");
		assert!(thumbnail_stored, "failed thumbnail remains stored");

		*storage.deny_delete.lock().expect("storage lock") = None;
		assert!(services.media.delete_owned_by(&mxc, user).await?, "retry remains owner-checked");
		assert!(services.media.get_metadata(&mxc).await.is_none(), "retry removes metadata");
		assert!(
			!services
				.media
				.mxc_is_owned_by_user(&mxc, user)
				.await,
			"retry removes ownership"
		);
		assert!(
			storage
				.objects
				.lock()
				.expect("storage lock")
				.is_empty(),
			"retry deletes the remaining thumbnail"
		);
		Ok(())
	}

	async fn storage_request(
		State(storage): State<Arc<Storage>>,
		method: Method,
		uri: Uri,
		body: Bytes,
	) -> Response {
		let key = uri
			.path()
			.strip_prefix("/sidecars/")
			.unwrap_or_default();
		match method {
			| Method::PUT => {
				storage
					.objects
					.lock()
					.expect("storage lock")
					.insert(key.to_owned(), body.to_vec());
				([(header::ETAG, "\"sidecar\"")], "").into_response()
			},
			| Method::HEAD => {
				let objects = storage.objects.lock().expect("storage lock");
				let Some(object) = objects.get(key) else {
					return StatusCode::NOT_FOUND.into_response();
				};
				(
					[
						(header::CONTENT_LENGTH, object.len().to_string()),
						(header::LAST_MODIFIED, "Wed, 01 Jan 2020 00:00:00 GMT".to_owned()),
						(header::ETAG, "\"sidecar\"".to_owned()),
					],
					"",
				)
					.into_response()
			},
			| Method::POST if uri.query() == Some("delete") => {
				let body = std::str::from_utf8(&body).expect("XML delete request");
				let key = body
					.split_once("<Key>")
					.expect("delete key")
					.1
					.split_once("</Key>")
					.expect("delete key end")
					.0;
				if storage
					.deny_delete
					.lock()
					.expect("storage lock")
					.as_deref() == Some(key)
				{
					return (
						StatusCode::FORBIDDEN,
						"<Error><Code>AccessDenied</Code><Message>Deletion \
						 denied</Message></Error>",
					)
						.into_response();
				}
				storage
					.objects
					.lock()
					.expect("storage lock")
					.remove(key);
				format!("<DeleteResult><Deleted><Key>{key}</Key></Deleted></DeleteResult>")
					.into_response()
			},
			| _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
		}
	}
}
