use std::{collections::BTreeMap, path::PathBuf, time::Instant};

use futures::{StreamExt, pin_mut};
use ruma::OwnedUserId;
use serde::Serialize;
use tuwunel_core::{Err, Result};

use super::{
	plan::{MapClass, RecordAction, map_class, plan_record, validate_ids},
	scan::{MapScan, ScanReport, scan_maps, write_report},
};
use crate::admin_command;

#[derive(Debug, Serialize)]
struct ApplyReport {
	from: String,
	to: String,
	/// Blocked records per map and reason; any entry means nothing was written.
	blocked: Vec<(String, &'static str, usize)>,
	rewritten: Vec<(String, usize)>,
	cleared: Vec<(String, usize)>,
	seconds: f64,
	post_scan: Option<ScanReport>,
}

#[admin_command]
pub(super) async fn user_id_rewrite_apply(
	&self,
	from: OwnedUserId,
	to: OwnedUserId,
	report: PathBuf,
	confirm_offline_clone: bool,
	rewrite_event_graph: bool,
) -> Result {
	let services = self.services;
	validate_ids(&from, &to, &services.server.name)?;
	if !confirm_offline_clone {
		return Err!("refusing to write without --confirm-offline-clone");
	}
	if services.db.is_read_only() {
		return Err!("refusing to write because the database is read-only");
	}
	for map in ["userid_password", "userid_origin"] {
		if services
			.db
			.get(map)?
			.get(to.as_bytes())
			.await
			.is_ok()
		{
			return Err!("refusing to write: {to} already exists ({map})");
		}
	}

	let timer = Instant::now();
	let (from_bytes, to_bytes) = (from.as_bytes(), to.as_bytes());
	let mut planned = Vec::new();
	let mut blocked: BTreeMap<(String, &'static str), usize> = BTreeMap::new();
	for (name, map) in services.db.iter() {
		let class = map_class(name);
		if class == MapClass::Skip {
			continue;
		}
		let mut actions = Vec::new();
		let stream = map.raw_stream();
		pin_mut!(stream);
		while let Some(item) = stream.next().await {
			let (key, value) = item?;
			match plan_record(class, key, value, from_bytes, to_bytes, rewrite_event_graph) {
				| RecordAction::Skip => {},
				| RecordAction::Block(reason) => {
					let count = blocked
						.entry(((*name).to_owned(), reason))
						.or_default();
					*count = count.saturating_add(1);
				},
				| action => actions.push((key.to_vec(), action)),
			}
		}
		if !actions.is_empty() {
			planned.push((*name, map, actions));
		}
	}

	let mut summary = ApplyReport {
		from: from.to_string(),
		to: to.to_string(),
		blocked: blocked
			.into_iter()
			.map(|((map, reason), count)| (map, reason, count))
			.collect(),
		rewritten: Vec::new(),
		cleared: Vec::new(),
		seconds: 0.0,
		post_scan: None,
	};
	if !summary.blocked.is_empty() {
		write_report(&report, &summary)?;
		return Err!("refusing to write: blocked records remain, see {}", report.display());
	}

	for (name, map, actions) in &planned {
		for (key, action) in actions {
			if let RecordAction::Rewrite { key: new_key, .. } = action
				&& new_key != key
				&& map.get(new_key).await.is_ok()
			{
				return Err!("refusing to write: target key collision in {name}");
			}
		}
	}

	let cork = services.db.cork_and_sync();
	for (name, map, actions) in planned {
		let (mut rewritten, mut cleared) = (0_usize, 0_usize);
		for (key, action) in actions {
			match action {
				| RecordAction::Rewrite { key: new_key, value } => {
					map.insert(&new_key, value);
					if new_key != key {
						map.remove(&key);
					}
					rewritten = rewritten.saturating_add(1);
				},
				| RecordAction::Clear => {
					map.remove(&key);
					cleared = cleared.saturating_add(1);
				},
				| RecordAction::Skip | RecordAction::Block(_) => {},
			}
		}
		if rewritten > 0 {
			summary
				.rewritten
				.push((name.to_owned(), rewritten));
		}
		if cleared > 0 {
			summary.cleared.push((name.to_owned(), cleared));
		}
	}
	drop(cork);
	services.db.engine.sort()?;
	summary.seconds = timer.elapsed().as_secs_f64();

	let post_scan = scan_maps(services, from_bytes, to_bytes, rewrite_event_graph, 3).await?;
	let remaining: usize = post_scan.maps.iter().map(MapScan::matches).sum();
	let rewritten: usize = summary
		.rewritten
		.iter()
		.map(|(_, count)| count)
		.sum();
	let cleared: usize = summary
		.cleared
		.iter()
		.map(|(_, count)| count)
		.sum();
	let seconds = summary.seconds;
	summary.post_scan = Some(post_scan);
	write_report(&report, &summary)?;

	write!(
		self,
		"rewrote {rewritten}, cleared {cleared} records in {seconds:.1}s; {remaining} matches \
		 remain, report at {}",
		report.display(),
	)
	.await
}
