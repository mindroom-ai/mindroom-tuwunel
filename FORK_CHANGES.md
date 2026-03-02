# MindRoom Tuwunel Fork - Changes Since d23f7f7e819681e25aac5ffa84897efdf2d8d43e

This document enumerates fork changes after base commit
`d23f7f7e819681e25aac5ffa84897efdf2d8d43e`.

Rules followed:
- No guessing. If intent is not explicit in commit message or diff, it is marked TODO.
- Each change lists files touched and observable behavior impact.

## How To Regenerate
- Commit list: `git log --reverse --format="%H %ad %s" d23f7f7e81..HEAD`
- Per-commit diff: `git show <sha>`
- File-level delta: `git diff --stat d23f7f7e81..HEAD`

## Working Tree (Not Yet Committed)
No uncommitted repository file changes documented.

## Commit-by-Commit Changes

### Add MindRoom compact-edit and purge configuration options
Commit:
- `fd521991d3ef8660883cb81ea02916f3478c9b72`

Files changed:
- `src/core/config/mod.rs`

What changed:
- Added new config fields:
  - `mindroom_compact_edits_enabled`
  - `mindroom_edit_purge_enabled`
  - `mindroom_edit_purge_min_age_secs`
  - `mindroom_edit_purge_interval_secs`
  - `mindroom_edit_purge_batch_size`
  - `mindroom_edit_purge_scan_limit`
  - `mindroom_edit_purge_dry_run`
- Added defaults for purge min-age, interval, batch size, and scan limit.

Why:
- Stated in commit subject.

### Port compact-edit collapsing in sync timelines
Commit:
- `b6dda1e2c26f2969042c58bb2351999e5d9eabdb`

Files changed:
- `src/api/client/sync/mod.rs`

What changed:
- Added collapse logic in `/sync` timeline loading path, gated by
  `mindroom_compact_edits_enabled`.
- Added collapse implementation that groups `m.replace` by
  `(target_event_id, sender)` and keeps only the newest entry per group.
- Added initial test coverage for collapse behavior.

Why:
- Stated in commit subject.

### Add background edit purge worker service
Commit:
- `0d216feb89385e692620770f7e4114b8de96123c`

Files changed:
- `src/service/edit_purge/mod.rs`
- `src/service/mod.rs`
- `src/service/services.rs`

What changed:
- Added `edit_purge` service module and wired it into global service startup.
- Implemented periodic purge worker:
  - incremental scan over `pduid_pdu`
  - superseded `m.replace` detection per `(target_event_id, sender)`
  - deletions from event/PDU index maps
  - dry-run mode support
- Added extensive service-level tests for purge behavior.

Why:
- Stated in commit subject.

### Use fallible map removal in purge path
Commit:
- `6fd81577081de09b8c6dfc8b45b586040b6f1bc4`

Files changed:
- `src/database/map/remove.rs`
- `src/service/edit_purge/mod.rs`

What changed:
- Added `Map::remove_fallible(...) -> Result` in database map layer.
- Updated purge path to call fallible removals and log failures instead of panicking.
- Changed purge delete flow to skip candidates when primary delete fails.

Why:
- Stated in commit subject.

### Validate non-zero purge interval and batch size
Commit:
- `d0863da7eb2cbb7d79e0ec05b001107bf9ccd7e9`

Files changed:
- `src/core/config/check.rs`

What changed:
- Added config validation errors for:
  - `mindroom_edit_purge_interval_secs == 0`
  - `mindroom_edit_purge_batch_size == 0`

Why:
- Stated in commit subject.

### Expand compact-edit sync tests
Commit:
- `7a6c9e9339c3f2038fa246b33fef76577cb96015`

Files changed:
- `src/api/client/sync/mod.rs`

What changed:
- Added additional sync collapse tests:
  - no-edit/single-edit behavior
  - multiple targets
  - order preservation
  - same-timestamp tie-breaks

Why:
- Stated in commit subject.

### Prefer timeline order over event timestamps
Commit:
- `d8c0cc9d461e6e5979083c5a5ef7a3f85a6645f1`

Files changed:
- `src/api/client/sync/mod.rs`
- `src/service/edit_purge/mod.rs`

What changed:
- Switched "latest edit wins" comparison from `origin_server_ts` to timeline/PDU order.
- Added tests for mismatched timestamp vs timeline-order cases.

Why:
- Stated in commit subject.

### Resolve compact-edits-port review findings
Commit:
- `86108c2b442822503f8cfd82132a98c447b609dc`

Files changed:
- `src/api/client/sync/mod.rs`
- `src/core/config/check.rs`
- `src/core/config/mod.rs`
- `src/core/matrix/event.rs`
- `src/core/matrix/event/relation.rs`
- `src/database/map/remove.rs`
- `src/service/edit_purge/mod.rs`

What changed:
- Moved shared relation extraction structs into core matrix event relation module and reused them.
- Fixed purge scan-limit docs and added `mindroom_edit_purge_scan_limit == 0` validation.
- Corrected `remove_fallible` flush error handling type.
- Added purge backlog queue persistence across cycles.
- Added cap/reset guard for in-memory latest-state map growth during long scan passes.

Why:
- Stated in commit subject and follow-up review context.

### Document fork options in sample config
Commit:
- `b8ecdd0cc0837919db081f2d93bdf4b1ec4eb04e`

Files changed:
- `tuwunel-example.toml`

What changed:
- Added commented documentation/examples for all MindRoom compact-edit and purge options.

Why:
- Stated in commit subject.

### Add purge backlog retry and backpressure hardening
Commit:
- `1913110fea8b0ad85c114239b65a1859e86a97fc`

Files changed:
- `src/service/edit_purge/mod.rs`

What changed:
- Added backlog cap and scan backpressure (skip scanning when backlog is full).
- Requeued failed delete candidates for retry on later cycles.
- Made `delete_event` fail fast on index cleanup failures (not just primary row failures).
- Extended batch-size test to verify multi-cycle backlog drain.

Why:
- Stated in commit subject.

## Runbook

## Purpose
Reduce sync payload noise for edit-heavy workloads and optionally reclaim storage
by deleting superseded historical edits after a grace period.

## Enable compact edit collapsing
In `tuwunel.toml`:

```toml
[global]
mindroom_compact_edits_enabled = true
```

Default is `false`.

## Enable edit purge worker
In `tuwunel.toml`:

```toml
[global]
mindroom_compact_edits_enabled = true
mindroom_edit_purge_enabled = true
mindroom_edit_purge_min_age_secs = 86400
mindroom_edit_purge_interval_secs = 3600
mindroom_edit_purge_batch_size = 1000
mindroom_edit_purge_scan_limit = 100000
mindroom_edit_purge_dry_run = false
```

## Recommended rollout
1. Enable `mindroom_compact_edits_enabled` first.
2. Enable purge with `mindroom_edit_purge_dry_run = true` and inspect logs.
3. Disable dry-run once behavior is confirmed.

## Behavior summary
- Compact mode keeps only latest superseding `m.replace` per `(target, sender)`
  in returned sync timeline batches.
- Purge mode deletes superseded edits from storage/index maps after min-age.
- Purge uses incremental scans with bounded queueing and retry behavior.

## Compatibility notes
- Client/event wire format remains standard Matrix.
- Intermediate historical edit events may not appear in client timelines once
  compact mode is enabled.
- Superseded old edits may be permanently removed when purge mode is enabled.
