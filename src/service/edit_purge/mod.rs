use std::{
	collections::{HashMap, HashSet, hash_map::Entry},
	sync::Arc,
	time::Duration,
};

use async_trait::async_trait;
use futures::StreamExt;
use ruma::{OwnedEventId, OwnedUserId};
use serde::Deserialize;
use tokio::{
	sync::{Mutex, Notify},
	time::{MissedTickBehavior, interval},
};
use tuwunel_core::{Result, Server, debug, info, matrix::pdu::PduEvent, utils, warn};
use tuwunel_database::{Database, Map};

pub struct Service {
	interval: Duration,
	interrupt: Notify,
	pduid_pdu: Arc<Map>,
	eventid_pduid: Arc<Map>,
	eventid_shorteventid: Arc<Map>,
	shorteventid_eventid: Arc<Map>,
	/// Last-scanned pdu_id key for incremental scanning.
	last_scan_key: Mutex<Option<Vec<u8>>>,
	/// Latest replacement seen so far for each (target, sender) during the
	/// current full-table scan pass. This is persisted across purge cycles so
	/// replacements split across scan windows are still compared.
	latest_replace_by_target_sender:
		Mutex<HashMap<(OwnedEventId, OwnedUserId), ReplaceCandidate>>,
	services: Services,
}

struct Services {
	server: Arc<Server>,
	db: Arc<Database>,
}

/// Extract m.relates_to from PDU content.
#[derive(Deserialize)]
struct ExtractRelatesToInfo {
	#[serde(rename = "m.relates_to")]
	relates_to: RelatesToInfo,
}

#[derive(Deserialize)]
struct RelatesToInfo {
	rel_type: String,
	event_id: OwnedEventId,
}

/// A candidate replacement event with its metadata.
#[derive(Clone)]
struct ReplaceCandidate {
	/// event_id for deterministic tie-breaks.
	event_id: OwnedEventId,
	/// The raw PduId bytes for deletion from pduid_pdu.
	pdu_id_bytes: Vec<u8>,
}

fn compare_replace_candidates(a: &ReplaceCandidate, b: &ReplaceCandidate) -> std::cmp::Ordering {
	a.pdu_id_bytes
		.cmp(&b.pdu_id_bytes)
		.then_with(|| a.event_id.cmp(&b.event_id))
}

fn next_scan_start_key(key: &[u8]) -> Vec<u8> {
	let mut next = Vec::with_capacity(key.len().saturating_add(1));
	next.extend_from_slice(key);
	next.push(0);
	next
}

#[async_trait]
impl crate::Service for Service {
	fn build(args: &crate::Args<'_>) -> Result<Arc<Self>> {
		let db = &args.db;
		Ok(Arc::new(Self {
			interval: Duration::from_secs(args.server.config.mindroom_edit_purge_interval_secs),
			interrupt: Notify::new(),
			pduid_pdu: db["pduid_pdu"].clone(),
			eventid_pduid: db["eventid_pduid"].clone(),
			eventid_shorteventid: db["eventid_shorteventid"].clone(),
			shorteventid_eventid: db["shorteventid_eventid"].clone(),
			last_scan_key: Mutex::new(None),
			latest_replace_by_target_sender: Mutex::new(HashMap::new()),
			services: Services {
				server: args.server.clone(),
				db: args.db.clone(),
			},
		}))
	}

	#[tracing::instrument(skip_all, name = "edit_purge", level = "debug")]
	async fn worker(self: Arc<Self>) -> Result {
		if !self.services.server.config.mindroom_edit_purge_enabled {
			debug!("MindRoom edit purge disabled");
			return Ok(());
		}

		if self.services.db.is_read_only() {
			warn!(
				"MindRoom edit purge enabled but database is read-only; skipping purge worker"
			);
			return Ok(());
		}

		info!(
			"MindRoom edit purge worker started (interval={}s, min_age={}s, batch={})",
			self.services.server.config.mindroom_edit_purge_interval_secs,
			self.services.server.config.mindroom_edit_purge_min_age_secs,
			self.services.server.config.mindroom_edit_purge_batch_size,
		);

		let mut i = interval(self.interval);
		i.set_missed_tick_behavior(MissedTickBehavior::Delay);
		i.reset_after(self.interval);
		let shutdown = self.services.server.until_shutdown();
		tokio::pin!(shutdown);
		loop {
			tokio::select! {
				() = self.interrupt.notified() => break,
				() = &mut shutdown => break,
				_ = i.tick() => (),
			}

			if let Err(e) = self.purge_cycle().await {
				warn!(%e, "MindRoom edit purge cycle failed");
			}
		}

		Ok(())
	}

	async fn interrupt(&self) { self.interrupt.notify_waiters(); }

	fn name(&self) -> &str { crate::service::make_name(std::module_path!()) }
}

impl Service {
	/// Run a single purge cycle: incrementally scan PDUs, find superseded
	/// m.replace events, and delete them with corked writes (batched flush),
	/// not as a single transactional unit.
	#[tracing::instrument(skip_all, level = "debug")]
	async fn purge_cycle(&self) -> Result {
		let config = &self.services.server.config;
		let min_age_ms = config.mindroom_edit_purge_min_age_secs.saturating_mul(1000);
		let batch_size = config.mindroom_edit_purge_batch_size;
		let scan_limit = config
			.mindroom_edit_purge_batch_size
			.saturating_mul(10)
			.max(config.mindroom_edit_purge_scan_limit);
		let dry_run = config.mindroom_edit_purge_dry_run;
		let now_ms: u64 = utils::millis_since_unix_epoch();
		let cutoff_ms = now_ms.saturating_sub(min_age_ms);

		// Phase 1: Incrementally scan PDUs from the last cursor position and
		// compare m.replace events against per-(target, sender) latest state that
		// persists across cycles during a full-table scan pass.
		let mut superseded_candidates: Vec<(OwnedEventId, ReplaceCandidate)> = Vec::new();
		let mut latest_replace_by_target_sender = self.latest_replace_by_target_sender.lock().await;

		let resume_key = self.last_scan_key.lock().await.clone();
		// raw_stream_from is inclusive, so move to the next lexicographic key
		// to avoid reprocessing the last key from the previous cycle.
		let resume_from_key = resume_key.as_deref().map(next_scan_start_key);

		let stream = if let Some(ref key) = resume_from_key {
			self.pduid_pdu.raw_stream_from(key).boxed()
		} else {
			self.pduid_pdu.raw_stream().boxed()
		};
		tokio::pin!(stream);

		let mut last_key: Option<Vec<u8>> = None;
		let mut scanned: usize = 0;
		let mut reached_end = true;

		while let Some(kv) = stream.next().await {
			let Ok((key, value)) = kv else {
				continue;
			};

			last_key = Some(key.to_vec());
			scanned += 1;

			let pdu: PduEvent = match serde_json::from_slice(&value) {
				| Ok(p) => p,
				| Err(_) => continue,
			};

			// Check if this is an m.replace event
			let Ok(content) = pdu.get_content::<ExtractRelatesToInfo>() else {
				continue;
			};
			if content.relates_to.rel_type != "m.replace" {
				continue;
			}

			// Check age: only consider events older than the cutoff
			let ts: u64 = pdu.origin_server_ts.into();
			if ts > cutoff_ms {
				continue;
			}

			let target_event_id = content.relates_to.event_id;
			let group_key = (target_event_id.clone(), pdu.sender.clone());
			let candidate = ReplaceCandidate {
				event_id: pdu.event_id.clone(),
				pdu_id_bytes: key.to_vec(),
			};

			match latest_replace_by_target_sender.entry(group_key) {
				| Entry::Vacant(entry) => {
					entry.insert(candidate);
				},
				| Entry::Occupied(mut entry) => {
					if compare_replace_candidates(&candidate, entry.get()).is_gt() {
						let superseded = std::mem::replace(entry.get_mut(), candidate);
						superseded_candidates.push((target_event_id, superseded));
					} else {
						superseded_candidates.push((target_event_id, candidate));
					}
				},
			}

			// Limit scan size per cycle to avoid blocking too long;
			// we'll resume from this position next cycle.
			if scanned >= scan_limit {
				reached_end = false;
				break;
			}
		}

		// Update the cursor: if we reached the end, reset to None (start
		// over next cycle). Otherwise, save the last key for resuming.
		{
			let mut cursor = self.last_scan_key.lock().await;
			if reached_end {
				*cursor = None;
				latest_replace_by_target_sender.clear();
			} else {
				*cursor = last_key;
			}
		}
		drop(latest_replace_by_target_sender);

		// Phase 2: Purge superseded events discovered during this scan window.
		let mut purge_count: usize = 0;
		let mut target_ids: HashSet<OwnedEventId> = HashSet::new();

		let _cork = if !dry_run {
			Some(self.services.db.cork_and_flush())
		} else {
			None
		};

		for (target_event_id, candidate) in superseded_candidates.into_iter().take(batch_size) {
			if dry_run {
				info!(
					event_id = %candidate.event_id,
					target = %target_event_id,
					"[dry-run] Would purge superseded edit"
				);
				purge_count += 1;
				target_ids.insert(target_event_id);
				continue;
			}

			if self.delete_event(&candidate) {
				debug!(
					event_id = %candidate.event_id,
					target = %target_event_id,
					"Purged superseded edit event"
				);
				purge_count += 1;
				target_ids.insert(target_event_id);
			}
		}

		let target_count = target_ids.len();

		if purge_count > 0 || target_count > 0 {
			info!(
				"MindRoom edit purge: {}purged {purge_count} superseded edits \
				 for {target_count} target events (scanned {scanned} PDUs)",
				if dry_run { "[dry-run] would have " } else { "" },
			);
		} else {
			debug!("MindRoom edit purge: no superseded edits found (scanned {scanned} PDUs)");
		}

		Ok(())
	}

	/// Delete a superseded edit event from the database.
	///
	/// Returns true when the primary event row was removed; returns false when
	/// purge should skip this candidate due to a write error.
	fn delete_event(&self, candidate: &ReplaceCandidate) -> bool {
		// Remove from pduid_pdu (the main event storage)
		if let Err(e) = self.pduid_pdu.remove_fallible(&candidate.pdu_id_bytes) {
			warn!(
				%e,
				event_id = %candidate.event_id,
				"Failed to remove superseded edit from pduid_pdu; skipping candidate"
			);
			return false;
		}

		// Remove from eventid_pduid (event_id -> pdu_id index)
		if let Err(e) = self
			.eventid_pduid
			.remove_fallible(candidate.event_id.as_bytes())
		{
			warn!(
				%e,
				event_id = %candidate.event_id,
				"Failed to remove superseded edit from eventid_pduid"
			);
		}

		// Remove from eventid_shorteventid / shorteventid_eventid
		// (short numeric ID mappings that would otherwise be orphaned)
		if let Ok(short_bytes) = self
			.eventid_shorteventid
			.get_blocking(candidate.event_id.as_bytes())
			&& let Err(e) = self.shorteventid_eventid.remove_fallible(&*short_bytes)
		{
			warn!(
				%e,
				event_id = %candidate.event_id,
				"Failed to remove superseded edit from shorteventid_eventid"
			);
		}

		if let Err(e) = self
			.eventid_shorteventid
			.remove_fallible(candidate.event_id.as_bytes())
		{
			warn!(
				%e,
				event_id = %candidate.event_id,
				"Failed to remove superseded edit from eventid_shorteventid"
			);
		}

		// Note: we intentionally do NOT remove from tofrom_relation.
		// The relation entry is tiny (16 bytes) and removing it could
		// affect federation or relation queries. The PDU itself being
		// gone is sufficient — lookups via the relation will simply
		// fail to find the PDU and skip it.
		true
	}
}

#[cfg(test)]
mod tests {
	use std::{
		collections::HashMap,
		fs,
		path::{Path, PathBuf},
		sync::{
			Arc,
			atomic::{AtomicU64, Ordering},
		},
		time::Duration,
	};

	use tracing::subscriber::NoSubscriber;
	use ruma::{EventId, OwnedEventId, OwnedRoomId, OwnedUserId, UInt};
	use serde_json::value::RawValue;
	use tokio::{
		sync::{Mutex, Notify},
		time::timeout,
	};
	use tuwunel_core::{
		Server,
		config::Config,
		log::{Logging, capture::State as CaptureState},
		matrix::pdu::{EventHash, PduEvent},
		utils,
	};
	use tuwunel_database::Database;

	use super::{Service, Services};

	static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(1);

	#[derive(Clone, Copy)]
	struct HarnessConfig {
		min_age_secs: u64,
		batch_size: usize,
		scan_limit: usize,
		dry_run: bool,
		enabled: bool,
		read_only: bool,
	}

	impl Default for HarnessConfig {
		fn default() -> Self {
			Self {
				min_age_secs: 0,
				batch_size: 100,
				scan_limit: 100_000,
				dry_run: false,
				enabled: true,
				read_only: false,
			}
		}
	}

	struct TestHarness {
		service: Arc<Service>,
		_temp_dir: PathBuf,
	}

	#[derive(Clone)]
	struct StoredEvent {
		event_id: OwnedEventId,
		pdu_key: Vec<u8>,
		short_key: Vec<u8>,
	}

	async fn make_harness(cfg: HarnessConfig) -> TestHarness {
		let temp_dir = unique_temp_dir();

		if cfg.read_only {
			let bootstrap_cfg = HarnessConfig {
				read_only: false,
				..cfg
			};
			let (bootstrap_server, bootstrap_db) = open_server_db(&temp_dir, bootstrap_cfg).await;
			drop(bootstrap_db);
			drop(bootstrap_server);
		}

		let (server, db) = open_server_db(&temp_dir, cfg).await;
		let service = Arc::new(Service {
			interval: Duration::from_secs(server.config.mindroom_edit_purge_interval_secs),
			interrupt: Notify::new(),
			pduid_pdu: db["pduid_pdu"].clone(),
			eventid_pduid: db["eventid_pduid"].clone(),
			eventid_shorteventid: db["eventid_shorteventid"].clone(),
			shorteventid_eventid: db["shorteventid_eventid"].clone(),
			last_scan_key: Mutex::new(None),
			latest_replace_by_target_sender: Mutex::new(HashMap::new()),
			services: Services { server, db },
		});

		TestHarness {
			service,
			_temp_dir: temp_dir,
		}
	}

	async fn open_server_db(temp_dir: &Path, cfg: HarnessConfig) -> (Arc<Server>, Arc<Database>) {
		let db_path = temp_dir.join("db");
		let config_path = temp_dir.join(if cfg.read_only {
			"tuwunel-read-only.toml"
		} else {
			"tuwunel.toml"
		});

		fs::create_dir_all(temp_dir).expect("create test temp dir");
		let config_contents = format!(
			r#"[global]
server_name = "example.com"
database_path = "{}"
mindroom_edit_purge_enabled = {}
mindroom_edit_purge_min_age_secs = {}
mindroom_edit_purge_interval_secs = 60
mindroom_edit_purge_batch_size = {}
mindroom_edit_purge_scan_limit = {}
mindroom_edit_purge_dry_run = {}
rocksdb_read_only = {}
"#,
			db_path.display(),
			cfg.enabled,
			cfg.min_age_secs,
			cfg.batch_size,
			cfg.scan_limit,
			cfg.dry_run,
			cfg.read_only,
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

	fn unique_temp_dir() -> PathBuf {
		let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
		let pid = std::process::id();
		let path = std::env::temp_dir().join(format!("tuwunel-edit-purge-{pid}-{id}"));
		fs::create_dir_all(&path).expect("create unique test dir");
		path
	}

	fn pdu_key(index: u32) -> Vec<u8> { format!("k{index:08}").into_bytes() }

	fn make_pdu(event_id: &str, sender: &str, ts: u64, replace_target: Option<&str>) -> PduEvent {
		let content = if let Some(target) = replace_target {
			format!(
				r#"{{"body":"edited","m.relates_to":{{"rel_type":"m.replace","event_id":"{target}"}}}}"#
			)
		} else {
			r#"{"body":"original"}"#.to_owned()
		};

		PduEvent {
			kind: ruma::events::TimelineEventType::RoomMessage,
			content: RawValue::from_string(content).expect("valid JSON content"),
			event_id: EventId::parse(event_id).expect("valid event id"),
			room_id: OwnedRoomId::try_from("!room:example.com").expect("valid room id"),
			sender: OwnedUserId::try_from(sender).expect("valid sender"),
			state_key: None,
			redacts: None,
			prev_events: Default::default(),
			auth_events: Default::default(),
			origin_server_ts: UInt::try_from(ts).expect("valid timestamp"),
			depth: UInt::try_from(1_u64).expect("valid depth"),
			hashes: EventHash::default(),
			origin: None,
			unsigned: None,
			signatures: None,
		}
	}

	fn insert_event(
		service: &Service,
		key_index: u32,
		event_id: &str,
		sender: &str,
		ts: u64,
		replace_target: Option<&str>,
	) -> StoredEvent {
		let pdu = make_pdu(event_id, sender, ts, replace_target);
		let event_id = pdu.event_id.clone();
		let key = pdu_key(key_index);
		let short_key = key_index.to_be_bytes().to_vec();
		let value = serde_json::to_vec(&pdu).expect("serialize pdu");

		service.pduid_pdu.insert(&key, value);
		service.eventid_pduid.insert(event_id.as_bytes(), &key);
		service
			.eventid_shorteventid
			.insert(event_id.as_bytes(), &short_key);
		service
			.shorteventid_eventid
			.insert(&short_key, event_id.as_bytes());

		StoredEvent {
			event_id,
			pdu_key: key,
			short_key,
		}
	}

	fn assert_event_present(service: &Service, event: &StoredEvent) {
		assert!(
			service.pduid_pdu.get_blocking(&event.pdu_key).is_ok(),
			"expected pduid_pdu entry for {}",
			event.event_id,
		);
		assert!(
			service
				.eventid_pduid
				.get_blocking(event.event_id.as_bytes())
				.is_ok(),
			"expected eventid_pduid entry for {}",
			event.event_id,
		);
		assert!(
			service
				.eventid_shorteventid
				.get_blocking(event.event_id.as_bytes())
				.is_ok(),
			"expected eventid_shorteventid entry for {}",
			event.event_id,
		);
		assert!(
			service
				.shorteventid_eventid
				.get_blocking(&event.short_key)
				.is_ok(),
			"expected shorteventid_eventid entry for {}",
			event.event_id,
		);
	}

	fn assert_event_absent(service: &Service, event: &StoredEvent) {
		assert!(
			service.pduid_pdu.get_blocking(&event.pdu_key).is_err(),
			"expected no pduid_pdu entry for {}",
			event.event_id,
		);
		assert!(
			service
				.eventid_pduid
				.get_blocking(event.event_id.as_bytes())
				.is_err(),
			"expected no eventid_pduid entry for {}",
			event.event_id,
		);
		assert!(
			service
				.eventid_shorteventid
				.get_blocking(event.event_id.as_bytes())
				.is_err(),
			"expected no eventid_shorteventid entry for {}",
			event.event_id,
		);
		assert!(
			service
				.shorteventid_eventid
				.get_blocking(&event.short_key)
				.is_err(),
			"expected no shorteventid_eventid entry for {}",
			event.event_id,
		);
	}

	#[tokio::test]
	async fn purge_basic_keeps_only_latest_edit() {
		let harness = make_harness(HarnessConfig::default()).await;
		let service = &harness.service;

		let target = insert_event(
			service,
			0,
			"$target_basic:example.com",
			"@alice:example.com",
			100,
			None,
		);
		let edit1 = insert_event(
			service,
			1,
			"$edit_basic_1:example.com",
			"@alice:example.com",
			1_000,
			Some("$target_basic:example.com"),
		);
		let edit2 = insert_event(
			service,
			2,
			"$edit_basic_2:example.com",
			"@alice:example.com",
			2_000,
			Some("$target_basic:example.com"),
		);
		let edit3 = insert_event(
			service,
			3,
			"$edit_basic_3:example.com",
			"@alice:example.com",
			3_000,
			Some("$target_basic:example.com"),
		);

		service.purge_cycle().await.expect("purge cycle succeeds");

		assert_event_present(service, &target);
		assert_event_absent(service, &edit1);
		assert_event_absent(service, &edit2);
		assert_event_present(service, &edit3);
	}

	#[tokio::test]
	async fn purge_prefers_pdu_order_over_timestamp() {
		let harness = make_harness(HarnessConfig::default()).await;
		let service = &harness.service;

		let target = insert_event(
			service,
			0,
			"$target_order:example.com",
			"@alice:example.com",
			100,
			None,
		);
		let newer_by_count_older_ts = insert_event(
			service,
			2,
			"$edit_order_keep:example.com",
			"@alice:example.com",
			1_000,
			Some("$target_order:example.com"),
		);
		let older_by_count_newer_ts = insert_event(
			service,
			1,
			"$edit_order_drop:example.com",
			"@alice:example.com",
			4_000,
			Some("$target_order:example.com"),
		);

		service.purge_cycle().await.expect("purge cycle succeeds");

		assert_event_present(service, &target);
		assert_event_present(service, &newer_by_count_older_ts);
		assert_event_absent(service, &older_by_count_newer_ts);
	}

	#[tokio::test]
	async fn purge_respects_min_age_threshold() {
		let harness = make_harness(HarnessConfig {
			min_age_secs: 60,
			..HarnessConfig::default()
		})
		.await;
		let service = &harness.service;
		let now_ms = utils::millis_since_unix_epoch();

		let target = insert_event(
			service,
			0,
			"$target_min_age:example.com",
			"@alice:example.com",
			now_ms.saturating_sub(2_000),
			None,
		);
		let recent_edit1 = insert_event(
			service,
			1,
			"$edit_recent_1:example.com",
			"@alice:example.com",
			now_ms.saturating_sub(1_000),
			Some("$target_min_age:example.com"),
		);
		let recent_edit2 = insert_event(
			service,
			2,
			"$edit_recent_2:example.com",
			"@alice:example.com",
			now_ms.saturating_sub(500),
			Some("$target_min_age:example.com"),
		);

		service.purge_cycle().await.expect("purge cycle succeeds");

		assert_event_present(service, &target);
		assert_event_present(service, &recent_edit1);
		assert_event_present(service, &recent_edit2);
	}

	#[tokio::test]
	async fn purge_groups_edits_by_sender() {
		let harness = make_harness(HarnessConfig::default()).await;
		let service = &harness.service;

		let target = insert_event(
			service,
			0,
			"$target_sender:example.com",
			"@alice:example.com",
			100,
			None,
		);
		let alice_edit1 = insert_event(
			service,
			1,
			"$edit_sender_alice_1:example.com",
			"@alice:example.com",
			1_000,
			Some("$target_sender:example.com"),
		);
		let bob_edit1 = insert_event(
			service,
			2,
			"$edit_sender_bob_1:example.com",
			"@bob:example.com",
			1_500,
			Some("$target_sender:example.com"),
		);
		let alice_edit2 = insert_event(
			service,
			3,
			"$edit_sender_alice_2:example.com",
			"@alice:example.com",
			2_000,
			Some("$target_sender:example.com"),
		);
		let bob_edit2 = insert_event(
			service,
			4,
			"$edit_sender_bob_2:example.com",
			"@bob:example.com",
			2_500,
			Some("$target_sender:example.com"),
		);

		service.purge_cycle().await.expect("purge cycle succeeds");

		assert_event_present(service, &target);
		assert_event_absent(service, &alice_edit1);
		assert_event_present(service, &alice_edit2);
		assert_event_absent(service, &bob_edit1);
		assert_event_present(service, &bob_edit2);
	}

	#[tokio::test]
	async fn purge_groups_edits_by_target() {
		let harness = make_harness(HarnessConfig::default()).await;
		let service = &harness.service;

		let target1 = insert_event(
			service,
			0,
			"$target_multi_a:example.com",
			"@alice:example.com",
			100,
			None,
		);
		let target2 = insert_event(
			service,
			1,
			"$target_multi_b:example.com",
			"@alice:example.com",
			200,
			None,
		);

		let target1_edit1 = insert_event(
			service,
			2,
			"$edit_multi_a_1:example.com",
			"@alice:example.com",
			1_000,
			Some("$target_multi_a:example.com"),
		);
		let target1_edit2 = insert_event(
			service,
			3,
			"$edit_multi_a_2:example.com",
			"@alice:example.com",
			2_000,
			Some("$target_multi_a:example.com"),
		);
		let target2_edit1 = insert_event(
			service,
			4,
			"$edit_multi_b_1:example.com",
			"@alice:example.com",
			1_500,
			Some("$target_multi_b:example.com"),
		);
		let target2_edit2 = insert_event(
			service,
			5,
			"$edit_multi_b_2:example.com",
			"@alice:example.com",
			2_500,
			Some("$target_multi_b:example.com"),
		);

		service.purge_cycle().await.expect("purge cycle succeeds");

		assert_event_present(service, &target1);
		assert_event_present(service, &target2);
		assert_event_absent(service, &target1_edit1);
		assert_event_present(service, &target1_edit2);
		assert_event_absent(service, &target2_edit1);
		assert_event_present(service, &target2_edit2);
	}

	#[tokio::test]
	async fn purge_single_edit_is_not_removed() {
		let harness = make_harness(HarnessConfig::default()).await;
		let service = &harness.service;

		let target = insert_event(
			service,
			0,
			"$target_single:example.com",
			"@alice:example.com",
			100,
			None,
		);
		let single_edit = insert_event(
			service,
			1,
			"$edit_single:example.com",
			"@alice:example.com",
			1_000,
			Some("$target_single:example.com"),
		);

		service.purge_cycle().await.expect("purge cycle succeeds");

		assert_event_present(service, &target);
		assert_event_present(service, &single_edit);
	}

	#[tokio::test]
	async fn purge_dry_run_does_not_delete_events() {
		let harness = make_harness(HarnessConfig {
			dry_run: true,
			..HarnessConfig::default()
		})
		.await;
		let service = &harness.service;

		let target = insert_event(
			service,
			0,
			"$target_dry_run:example.com",
			"@alice:example.com",
			100,
			None,
		);
		let edit1 = insert_event(
			service,
			1,
			"$edit_dry_run_1:example.com",
			"@alice:example.com",
			1_000,
			Some("$target_dry_run:example.com"),
		);
		let edit2 = insert_event(
			service,
			2,
			"$edit_dry_run_2:example.com",
			"@alice:example.com",
			2_000,
			Some("$target_dry_run:example.com"),
		);

		service.purge_cycle().await.expect("purge cycle succeeds");

		assert_event_present(service, &target);
		assert_event_present(service, &edit1);
		assert_event_present(service, &edit2);
	}

	#[tokio::test]
	async fn purge_respects_batch_size_limit() {
		let harness = make_harness(HarnessConfig {
			batch_size: 2,
			..HarnessConfig::default()
		})
		.await;
		let service = &harness.service;

		let target = insert_event(
			service,
			0,
			"$target_batch:example.com",
			"@alice:example.com",
			100,
			None,
		);
		let edit1 = insert_event(
			service,
			1,
			"$edit_batch_1:example.com",
			"@alice:example.com",
			1_000,
			Some("$target_batch:example.com"),
		);
		let edit2 = insert_event(
			service,
			2,
			"$edit_batch_2:example.com",
			"@alice:example.com",
			2_000,
			Some("$target_batch:example.com"),
		);
		let edit3 = insert_event(
			service,
			3,
			"$edit_batch_3:example.com",
			"@alice:example.com",
			3_000,
			Some("$target_batch:example.com"),
		);
		let edit4 = insert_event(
			service,
			4,
			"$edit_batch_4:example.com",
			"@alice:example.com",
			4_000,
			Some("$target_batch:example.com"),
		);

		service.purge_cycle().await.expect("purge cycle succeeds");

		assert_event_present(service, &target);
		assert_event_absent(service, &edit1);
		assert_event_absent(service, &edit2);
		assert_event_present(service, &edit3);
		assert_event_present(service, &edit4);
	}

	#[tokio::test]
	async fn purge_across_scan_windows_persists_latest_state() {
		let harness = make_harness(HarnessConfig {
			scan_limit: 1_000,
			..HarnessConfig::default()
		})
		.await;
		let service = &harness.service;

		let target = insert_event(
			service,
			0,
			"$target_windows:example.com",
			"@alice:example.com",
			100,
			None,
		);
		let older_edit = insert_event(
			service,
			1,
			"$edit_windows_old:example.com",
			"@alice:example.com",
			1_000,
			Some("$target_windows:example.com"),
		);

		{
			let _cork = service.services.db.cork_and_flush();
			for i in 2..=1_000_u32 {
				service.pduid_pdu.insert(&pdu_key(i), br#"not-json"#);
			}
		}

		let newer_edit = insert_event(
			service,
			1_001,
			"$edit_windows_new:example.com",
			"@alice:example.com",
			2_000,
			Some("$target_windows:example.com"),
		);

		service.purge_cycle().await.expect("first purge cycle succeeds");
		assert_event_present(service, &target);
		assert_event_present(service, &older_edit);
		assert!(service.last_scan_key.lock().await.is_some());

		service
			.purge_cycle()
			.await
			.expect("second purge cycle succeeds");

		assert_event_absent(service, &older_edit);
		assert_event_present(service, &newer_edit);
		assert!(service.last_scan_key.lock().await.is_none());
	}

	#[tokio::test]
	async fn worker_exits_gracefully_when_database_is_read_only() {
		let harness = make_harness(HarnessConfig {
			read_only: true,
			..HarnessConfig::default()
		})
		.await;

		let result = timeout(
			Duration::from_secs(1),
			<Service as crate::Service>::worker(harness.service.clone()),
		)
		.await;

		assert!(result.is_ok(), "worker timed out on read-only database");
		assert!(
			result.expect("timeout already checked").is_ok(),
			"worker should return Ok on read-only database"
		);
	}
}
