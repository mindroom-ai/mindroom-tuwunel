use crate::{Config, Err, Result};

pub(super) fn check(config: &Config) -> Result {
	if config.mindroom_edit_purge_interval_secs == 0 {
		return Err!(Config(
			"mindroom_edit_purge_interval_secs",
			"mindroom_edit_purge_interval_secs must be at least 1 second."
		));
	}

	if config.mindroom_edit_purge_batch_size == 0 {
		return Err!(Config(
			"mindroom_edit_purge_batch_size",
			"mindroom_edit_purge_batch_size must be at least 1."
		));
	}

	if config.mindroom_edit_purge_scan_limit == 0 {
		return Err!(Config(
			"mindroom_edit_purge_scan_limit",
			"mindroom_edit_purge_scan_limit must be at least 1."
		));
	}

	if config
		.default_power_level_content_override
		.as_ref()
		.is_some_and(|value| !value.is_object())
	{
		return Err!(Config(
			"default_power_level_content_override",
			"default_power_level_content_override must be a TOML table / JSON object."
		));
	}

	Ok(())
}
