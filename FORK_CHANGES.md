# MindRoom Tuwunel Fork - Changes Since `d23f7f7e819681e25aac5ffa84897efdf2d8d43e`

This document describes the current fork behavior and code paths added on top of
base commit `d23f7f7e819681e25aac5ffa84897efdf2d8d43e`.

## How To Inspect
- Commit range: `git log --reverse --oneline d23f7f7e81..HEAD`
- Files changed in the fork: `git diff --stat d23f7f7e81..HEAD`
- Per-commit patch: `git show <sha>`

## Fork Commit Set

### 1) `mindroom/config: add compact-edit + purge config, validation, and example docs`
Files:
- `src/core/config/mod.rs`
- `src/core/config/check.rs`
- `tuwunel-example.toml`

Behavior:
- Adds MindRoom controls for edit compaction and edit purge.
- Adds validation for purge settings (`interval`, `batch_size`, `scan_limit`) to reject zero values.
- Documents all MindRoom options in the sample config.

### 2) `core/event: share m.relates_to extraction helpers`
Files:
- `src/core/matrix/event.rs`
- `src/core/matrix/event/relation.rs`

Behavior:
- Centralizes relation extraction types used by both sync compaction and purge logic.
- Avoids duplicate ad-hoc relation parsing structs.

### 3) `sync: compact edit collapsing by timeline order with full tests`
Files:
- `src/api/client/sync/mod.rs`

Behavior:
- When `mindroom_compact_edits_enabled = true`, `/sync` timeline responses collapse
  multiple `m.replace` events per `(target_event_id, sender)` down to the latest one.
- "Latest" is determined by timeline order (`PduCount`), not sender-supplied event
  timestamps, with `event_id` as deterministic tie-break.
- Includes expanded tests for no-edit/single-edit/multi-target/same-ts/timeline-order
  behavior.

### 4) `service/edit_purge: background purge worker with resilient deletes and backlog handling`
Files:
- `src/service/edit_purge/mod.rs`
- `src/service/mod.rs`
- `src/service/services.rs`
- `src/database/map/remove.rs`

Behavior:
- Adds a background worker that scans for superseded `m.replace` events and purges
  old ones from storage and indexes.
- Uses incremental scanning with a resume cursor (`last_scan_key`) and bounded scan
  work per cycle.
- Uses backlog queueing and retry logic for failed deletes.
- Uses `remove_fallible(...)` map operations for explicit error handling instead of
  panicking in purge paths.
- Honors:
  - `mindroom_edit_purge_enabled`
  - `mindroom_edit_purge_min_age_secs`
  - `mindroom_edit_purge_interval_secs`
  - `mindroom_edit_purge_batch_size`
  - `mindroom_edit_purge_scan_limit`
  - `mindroom_edit_purge_dry_run`

### 5) `docs: add fork overview and FORK_CHANGES runbook`
Files:
- `README.md`
- `FORK_CHANGES.md`

Behavior:
- Adds fork-focused documentation and runbook entry points.

### 6) `ci: add GitHub release workflow for ARM and x86_64 binaries`
Files:
- `.github/workflows/mindroom-release.yml`

Behavior:
- Adds release automation for MindRoom fork binaries targeting ARM and x86_64.

### 7) `oauth: fall back to Apple id_token claims when userinfo fails`
Files:
- `src/api/client/session/sso.rs`
- `src/service/oauth/sessions.rs`

Behavior:
- For Apple OAuth flows, if userinfo endpoint lookup fails, the server falls back to
  claims from `id_token` so login can still complete.

### 8) `ci(release): auto-tag main pushes with mindroom suffix`
Files:
- `.github/workflows/auto-mindroom-release.yml`
- `scripts/fork_release_tag.py`
- `README.md`
- `FORK_CHANGES.md`

Behavior:
- Adds an automatic tagger on `main` pushes that computes
  `v<base_version>-mindroom.<n>`.
- Reads `base_version` from `BASE_VERSION`, then `Cargo.toml` workspace version,
  then semver base tags as fallback.
- Reuses an existing release tag when `HEAD` is already tagged for that base
  version/iteration.
- Pushes the computed tag so existing tag-driven release pipelines can publish
  artifacts.

## Runtime Configuration

### Compact edits in `/sync`
```toml
[global]
mindroom_compact_edits_enabled = true
```

### Background purge of superseded edits
```toml
[global]
mindroom_edit_purge_enabled = true
mindroom_edit_purge_min_age_secs = 86400
mindroom_edit_purge_interval_secs = 3600
mindroom_edit_purge_batch_size = 1000
mindroom_edit_purge_scan_limit = 100000
mindroom_edit_purge_dry_run = false
```

## Recommended Rollout
1. Enable `mindroom_compact_edits_enabled` first.
2. Enable purge in dry-run mode (`mindroom_edit_purge_dry_run = true`).
3. Inspect logs for candidate volume and purge cadence.
4. Disable dry-run when behavior is confirmed.

## Behavior Summary
- Sync compaction reduces edit-heavy timeline payloads by returning only latest
  replacements per `(target, sender)`.
- Purge worker reclaims storage by removing superseded historical edits after the
  configured age threshold.
- OAuth change improves Apple sign-in robustness when userinfo retrieval is unavailable.
- Main-branch pushes now auto-generate MindRoom-suffixed release tags.

## Compatibility Notes
- Matrix event formats remain standard.
- Clients may observe fewer intermediate edit events in `/sync` timelines when
  compact mode is enabled.
- Superseded edits can be permanently removed when purge mode is enabled.
