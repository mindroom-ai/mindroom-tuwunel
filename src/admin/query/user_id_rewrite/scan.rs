use std::{
	ffi::OsString,
	fs::{self, OpenOptions},
	io::{BufWriter, Write},
	os::unix::fs::OpenOptionsExt,
	path::{Path, PathBuf},
};

use futures::{StreamExt, pin_mut};
use ruma::OwnedUserId;
use serde::Serialize;
use tuwunel_core::{Err, Result};
use tuwunel_service::Services;

use super::plan::{
	MapClass, RecordAction, find_user_id, localpart_only_hits, map_class, plan_record,
	sample_allowed, url_encoded_hits, validate_ids,
};
use crate::admin_command;

#[derive(Debug, Serialize)]
pub(super) struct ScanReport {
	pub(super) from: String,
	pub(super) to: String,
	/// Occurrences of the target ID anywhere; non-zero before a rewrite means
	/// the target is already referenced.
	pub(super) to_occurrences: usize,
	pub(super) blocked_records: usize,
	pub(super) maps: Vec<MapScan>,
}

#[derive(Debug, Serialize)]
pub(super) struct MapScan {
	pub(super) map: String,
	pub(super) class: MapClass,
	pub(super) key_matches: usize,
	pub(super) value_matches: usize,
	pub(super) rewrite: usize,
	pub(super) clear: usize,
	pub(super) block: usize,
	pub(super) block_reasons: Vec<&'static str>,
	pub(super) localpart_only: usize,
	pub(super) url_encoded: usize,
	pub(super) samples: Vec<String>,
}

impl MapScan {
	pub(super) fn matches(&self) -> usize {
		self.key_matches
			.saturating_add(self.value_matches)
	}
}

#[admin_command]
pub(super) async fn user_id_rewrite_scan(
	&self,
	from: OwnedUserId,
	to: OwnedUserId,
	report: PathBuf,
	sample_limit_per_map: usize,
) -> Result {
	validate_ids(&from, &to, &self.services.server.name)?;
	let scan =
		scan_maps(self.services, from.as_bytes(), to.as_bytes(), true, sample_limit_per_map)
			.await?;
	write_report(&report, &scan)?;

	let with_matches = scan
		.maps
		.iter()
		.filter(|map| map.matches() > 0)
		.count();
	write!(
		self,
		"scanned {} maps, {with_matches} with matches, {} blocked records, report at {}",
		self.services.db.iter().count(),
		scan.blocked_records,
		report.display(),
	)
	.await
}

/// Walk every map and plan each record that references `from`.
pub(super) async fn scan_maps(
	services: &Services,
	from: &[u8],
	to: &[u8],
	rewrite_event_graph: bool,
	sample_limit_per_map: usize,
) -> Result<ScanReport> {
	let mut report = ScanReport {
		from: String::from_utf8_lossy(from).into_owned(),
		to: String::from_utf8_lossy(to).into_owned(),
		to_occurrences: 0,
		blocked_records: 0,
		maps: Vec::new(),
	};

	for (name, map) in services.db.iter() {
		let class = map_class(name);
		if class == MapClass::Skip {
			continue;
		}
		let mut scan = MapScan {
			map: (*name).to_owned(),
			class,
			key_matches: 0,
			value_matches: 0,
			rewrite: 0,
			clear: 0,
			block: 0,
			block_reasons: Vec::new(),
			localpart_only: 0,
			url_encoded: 0,
			samples: Vec::new(),
		};

		let stream = map.raw_stream();
		pin_mut!(stream);
		while let Some(item) = stream.next().await {
			let (key, value) = item?;
			report.to_occurrences = report
				.to_occurrences
				.saturating_add(find_user_id(key, to).len())
				.saturating_add(find_user_id(value, to).len());
			scan.localpart_only = scan
				.localpart_only
				.saturating_add(localpart_only_hits(key, from))
				.saturating_add(localpart_only_hits(value, from));
			scan.url_encoded = scan
				.url_encoded
				.saturating_add(url_encoded_hits(value, from));

			let key_matches = find_user_id(key, from).len();
			let value_matches = find_user_id(value, from).len();
			if key_matches == 0 && value_matches == 0 {
				continue;
			}

			scan.key_matches = scan.key_matches.saturating_add(key_matches);
			scan.value_matches = scan.value_matches.saturating_add(value_matches);
			match plan_record(class, key, value, from, to, rewrite_event_graph) {
				| RecordAction::Skip => {},
				| RecordAction::Rewrite { .. } => scan.rewrite = scan.rewrite.saturating_add(1),
				| RecordAction::Clear => scan.clear = scan.clear.saturating_add(1),
				| RecordAction::Block(reason) => {
					scan.block = scan.block.saturating_add(1);
					if !scan.block_reasons.contains(&reason) {
						scan.block_reasons.push(reason);
					}
				},
			}
			if sample_allowed(name, class) && scan.samples.len() < sample_limit_per_map {
				scan.samples.push(sample(key, value, from));
			}
		}

		report.blocked_records = report.blocked_records.saturating_add(scan.block);
		if scan.matches() > 0 || scan.localpart_only > 0 || scan.url_encoded > 0 {
			report.maps.push(scan);
		}
	}

	Ok(report)
}

/// Reports can hold message excerpts, so only the owner may read them. The
/// report is built in a new mode 0600 file next to the target and renamed over
/// it, so an existing report never keeps its old mode and a failed write never
/// leaves a truncated report behind.
pub(super) fn write_report<T: Serialize>(path: &Path, report: &T) -> Result {
	let Some(name) = path.file_name() else {
		return Err!("report path {} has no file name", path.display());
	};

	let mut temp_name = OsString::from(".");
	temp_name.push(name);
	temp_name.push(format!(".{}.tmp", std::process::id()));
	let temp = path.with_file_name(temp_name);

	let file = OpenOptions::new()
		.write(true)
		.create_new(true)
		.mode(0o600)
		.open(&temp)?;

	let result = write_json(&file, report)
		.and_then(|()| Ok(file.sync_all()?))
		.and_then(|()| Ok(fs::rename(&temp, path)?));
	if result.is_err() {
		drop(fs::remove_file(&temp));
	}

	result
}

/// Flushes explicitly: dropping a `BufWriter` discards a flush error.
fn write_json<W: Write, T: Serialize>(writer: W, report: &T) -> Result {
	let mut writer = BufWriter::new(writer);
	serde_json::to_writer_pretty(&mut writer, report)?;
	writer.flush()?;

	Ok(())
}

/// Escaped key plus a window of the value around the first match.
fn sample(key: &[u8], value: &[u8], from: &[u8]) -> String {
	const WINDOW: usize = 80;
	let start = find_user_id(value, from)
		.first()
		.map_or(0, |offset| offset.saturating_sub(WINDOW));
	let end = value.len().min(
		start
			.saturating_add(from.len())
			.saturating_add(2 * WINDOW),
	);
	let key = &key[..key.len().min(160)];

	format!("key={} value={}", key.escape_ascii(), value[start..end].escape_ascii())
}

#[cfg(test)]
mod tests {
	use std::{
		fs,
		io::{self, Write},
		os::unix::fs::PermissionsExt,
		path::PathBuf,
	};

	use serde::{Serialize, Serializer};

	use super::{write_json, write_report};

	/// Fresh directory per test so parallel tests and leftovers cannot clash.
	fn test_dir(name: &str) -> PathBuf {
		let dir =
			std::env::temp_dir().join(format!("user-id-rewrite-{}-{name}", std::process::id()));
		drop(fs::remove_dir_all(&dir));
		fs::create_dir_all(&dir).unwrap();
		dir
	}

	fn mode(path: &PathBuf) -> u32 { fs::metadata(path).unwrap().permissions().mode() & 0o777 }

	struct Unserializable;

	impl Serialize for Unserializable {
		fn serialize<S: Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
			Err(serde::ser::Error::custom("cannot serialize"))
		}
	}

	/// Accepts writes but fails on flush, like a full disk behind a buffer.
	struct FailingFlush;

	impl Write for FailingFlush {
		fn write(&mut self, buf: &[u8]) -> io::Result<usize> { Ok(buf.len()) }

		fn flush(&mut self) -> io::Result<()> { Err(io::Error::other("flush failed")) }
	}

	#[test]
	fn reports_are_private() {
		let dir = test_dir("private");
		let path = dir.join("report.json");
		write_report(&path, &serde_json::json!({"ok": true})).unwrap();
		assert_eq!(mode(&path), 0o600);
		fs::remove_dir_all(&dir).unwrap();
	}

	#[test]
	fn existing_report_becomes_private_with_new_content() {
		let dir = test_dir("existing");
		let path = dir.join("report.json");
		fs::write(&path, "old report with a much longer body than the new one").unwrap();
		fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
		write_report(&path, &serde_json::json!({"ok": true})).unwrap();
		assert_eq!(mode(&path), 0o600);
		let report: serde_json::Value =
			serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
		assert_eq!(report, serde_json::json!({"ok": true}));
		let entries = fs::read_dir(&dir).unwrap().count();
		assert_eq!(entries, 1, "no temporary file may stay behind");
		fs::remove_dir_all(&dir).unwrap();
	}

	#[test]
	fn failed_write_keeps_old_report_and_leaves_no_temporary_file() {
		let dir = test_dir("failed");
		let path = dir.join("report.json");
		fs::write(&path, "old report").unwrap();
		write_report(&path, &Unserializable).unwrap_err();
		assert_eq!(fs::read_to_string(&path).unwrap(), "old report");
		let entries = fs::read_dir(&dir).unwrap().count();
		assert_eq!(entries, 1, "no temporary file may stay behind");
		fs::remove_dir_all(&dir).unwrap();
	}

	#[test]
	fn flush_error_is_not_swallowed() {
		write_json(FailingFlush, &serde_json::json!({"ok": true})).unwrap_err();
	}
}
