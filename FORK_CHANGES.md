# MindRoom Tuwunel Fork - Changes Since `v1.5.1`

This document describes the current MindRoom fork behavior and operational
changes on top of upstream `v1.5.1`.

## How To Inspect
- Commit range: `git log --reverse --oneline v1.5.1..HEAD`
- Files changed in the fork: `git diff --stat v1.5.1..HEAD`
- Runtime-only changes: `git diff --stat v1.5.1..HEAD -- src/ tuwunel-example.toml`
- Per-commit patch: `git show <sha>`

## Intended Commit History

### Runtime Changes

#### 1) `mindroom/edits: compact /sync and purge superseded edits`
Files:
- `src/api/client/sync/mod.rs`
- `src/core/config/check.rs`
- `src/core/config/mod.rs`
- `src/core/matrix/event.rs`
- `src/core/matrix/event/relation.rs`
- `src/database/map/remove.rs`
- `src/service/edit_purge/mod.rs`
- `src/service/mod.rs`
- `src/service/services.rs`
- `tuwunel-example.toml`

Behavior:
- Adds `/sync` timeline compaction for superseded `m.replace` events.
- Adds a background purge worker that deletes old superseded edit events from
  storage and indexes.
- Adds the MindRoom edit lifecycle configuration surface and purge validation.
- Shares relation extraction helpers used by both sync compaction and purge
  logic.

#### 2) `oauth: fall back to Apple id_token claims when userinfo fails`
Files:
- `src/api/client/session/sso.rs`
- `src/service/oauth/sessions.rs`

Behavior:
- For Apple OAuth flows, if the provider `userinfo` request fails, the server
  decodes claims from `id_token` so login can still complete.

#### 3) `auth/uiaa: add strict-CSP-safe SSO fallback flow`
Files:
- `src/api/client/account.rs`
- `src/api/client/mod.rs`
- `src/api/client/session/sso.rs`
- `src/api/client/uiaa.rs`
- `src/api/router.rs`
- `src/api/router/auth/uiaa.rs`
- `src/database/maps.rs`
- `src/service/uiaa/mod.rs`
- `src/service/users/mod.rs`

Behavior:
- Adds an SSO-based UIAA fallback flow for SSO-origin users.
- Uses server redirects and completion endpoints that work under strict CSP.
- Persists UIAA session reverse lookups so fallback completion survives restart.
- Repairs legacy SSO-origin metadata where older flows left accounts marked as
  `password` users.

#### 4) `users/sso: reactivate self-deactivated accounts on login`
Files:
- `src/admin/user/commands.rs`
- `src/api/client/account.rs`
- `src/api/client/membership/mod.rs`
- `src/api/client/session/sso.rs`
- `src/database/maps.rs`
- `src/service/deactivate/mod.rs`
- `src/service/emergency/mod.rs`
- `src/service/users/mod.rs`

Behavior:
- Reactivates a deactivated local SSO account when the same identity logs in
  again.
- Restricts that reactivation to accounts that were self-deactivated, not
  admin-deactivated.
- Persists a deactivation reason so policy decisions can distinguish self
  service from administrative actions.

### Operational Changes

#### 5) `ci: add GitHub release workflow for ARM and x86_64 binaries`
Files:
- `.github/workflows/mindroom-release.yml`

Behavior:
- Adds tagged binary publishing for Linux `x86_64` and `aarch64`.

#### 6) `ci(release): auto-tag main pushes and create releases`
Files:
- `.github/workflows/auto-mindroom-release.yml`
- `scripts/fork_release_tag.py`

Behavior:
- Computes `v<base_version>-mindroom.<n>` tags on `main`.
- Creates or reuses the corresponding GitHub Release.

#### 7) `ci(container): publish release containers`
Files:
- `.github/workflows/auto-mindroom-release.yml`
- `.github/workflows/mindroom-container-release.yml`
- `docker/bake.sh`

Behavior:
- Dispatches container publication for MindRoom release tags.
- Uses the configured buildx builder for release container builds.

#### 8) `docs: summarize fork runtime and release additions`
Files:
- `README.md`

Behavior:
- Adds a concise fork overview to the README and links readers to this
  runbook.

## Runtime Configuration

### Edit compaction in `/sync`
```toml
[global]
mindroom_compact_edits_enabled = true
```

### Purge of superseded edits
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
2. Enable purge in dry-run mode with `mindroom_edit_purge_dry_run = true`.
3. Inspect logs for candidate volume and purge cadence.
4. Disable dry-run when behavior is confirmed.

## Behavior Summary
- Edit lifecycle changes reduce redundant edit traffic in `/sync` and can
  reclaim storage by purging superseded historical edits.
- Apple OAuth fallback improves sign-in robustness when `userinfo` is
  unavailable.
- UIAA SSO fallback supports strict-CSP deployments that cannot rely on inline
  browser logic in the default flow.
- Returning SSO users are reactivated only when they self-deactivated.

## Operational Summary
- Main-branch pushes can auto-create MindRoom release tags and GitHub Releases.
- Tagged releases publish Linux binaries for `x86_64` and `aarch64`.
- Release tags can also trigger container publication.

## Compatibility Notes
- Matrix event formats remain standard.
- Clients may observe fewer intermediate edit events in `/sync` when compact
  mode is enabled.
- Superseded edits can be permanently removed when purge is enabled.
- Admin-deactivated SSO accounts stay deactivated on future login attempts.
