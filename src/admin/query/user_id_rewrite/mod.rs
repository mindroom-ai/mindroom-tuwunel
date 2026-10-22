mod apply;
mod plan;
mod scan;

use std::path::PathBuf;

use clap::Subcommand;
use ruma::OwnedUserId;
use tuwunel_core::Result;

use crate::admin_command_dispatch;

#[admin_command_dispatch(handler_prefix = "user_id_rewrite")]
#[derive(Debug, Subcommand)]
/// Rename one local user ID in place, on an offline copy of the database
pub(crate) enum UserIdRewriteCommand {
	/// - Report every occurrence of a user ID and the planned action, without
	///   writing
	Scan {
		/// Existing local user ID
		#[arg(long)]
		from: OwnedUserId,

		/// New local user ID
		#[arg(long)]
		to: OwnedUserId,

		/// Path of the JSON report to write
		#[arg(long)]
		report: PathBuf,

		/// Sample records to include for each matching map
		#[arg(long, default_value_t = 3)]
		sample_limit_per_map: usize,
	},

	/// - Rewrite the user ID in place; refuses unless every matching record has
	///   a planned action
	Apply {
		/// Existing local user ID
		#[arg(long)]
		from: OwnedUserId,

		/// New local user ID; must not exist yet
		#[arg(long)]
		to: OwnedUserId,

		/// Path of the JSON report to write
		#[arg(long)]
		report: PathBuf,

		/// Required: this database is an offline copy, not a live server
		#[arg(long)]
		confirm_offline_clone: bool,

		/// Also rewrite historical events (pduid_pdu and related maps)
		#[arg(long)]
		rewrite_event_graph: bool,
	},
}
