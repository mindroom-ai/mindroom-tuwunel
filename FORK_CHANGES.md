# MindRoom Tuwunel Fork - Changes vs Upstream

This document describes the current MindRoom fork behavior on top of upstream
`tuwunel`. The fork is rebased directly onto upstream commits; see
`docs/rebase-*.md` for the per-rebase log. As of the 2026-07-05 rebase the base
is upstream `main` just past `v1.8.0` (which is the first upstream code to carry
native `m.replace` edit bundling, MSC3925).

## How To Inspect
- Fork commits: `git log --reverse --oneline <upstream-base>..HEAD`
- Files changed in the fork: `git diff --stat <upstream-base>..HEAD`
- Per-commit patch: `git show <sha>`

## Runtime Changes

### 1) `mindroom/edits: compact /sync, purge superseded edits, bundle the survivor`
Files:
- `src/api/client/sync/mod.rs`, `src/api/client/sync/mindroom_edits.rs`
- `src/core/config/mod.rs`, `src/core/config/check/mindroom.rs`, `src/core/config/check.rs`
- `src/core/matrix/event.rs`, `src/core/matrix/event/relation.rs`
- `src/database/map/remove.rs`
- `src/service/edit_purge/mod.rs`, `src/service/mod.rs`, `src/service/services.rs`
- `src/mindroom-tests/tests/edit_purge_bundle_compose.rs`
- `tuwunel-example.toml`

Behavior:
- Adds `/sync` timeline compaction for superseded `m.replace` events.
- Adds a background purge worker that deletes old superseded edit events from
  storage and indexes, keeping exactly one edit per (target, sender).
- Adds the MindRoom edit-lifecycle configuration surface and purge validation.
- **Turns on upstream's edit bundling by default** (`bundle_edit_relations`,
  MSC3925; upstream ships it off). The purge deletes superseded edits, so
  without the bundle a history endpoint (`/messages`, `/context`, `/event`, ...)
  would serve an original with its stale pre-edit body and no way for the client
  to find the surviving edit. Upstream bundles the newest surviving edit onto
  the original at `unsigned.m.relations.m.replace` via its `relatesto_typed`
  typed index; the purge composes with that index (it tolerates the dangling
  rows the purge leaves behind and always selects the surviving edit), and
  upstream's startup `rebuild_relatesto_typed` migration indexes pre-existing
  edits. `edit_purge::purge_cycle` is `pub` so operators/tests can trigger a
  cycle; a composition test drives a real purge and asserts the survivor is
  still bundled and that a dangling newest index row is skipped.

### 2) `auth/sso: strict-CSP SSO UIAA fallback, self-reactivation, cookie hardening`
Files:
- `src/api/client/account.rs` (and `account/*`), `src/api/client/mod.rs`
- `src/api/client/session/sso.rs`, `src/api/client/uiaa.rs`
- `src/api/router.rs`, `src/api/router/auth/uiaa.rs`
- `src/admin/user/*`, `src/api/oidc/account/account_deactivate.rs`
- `src/database/maps.rs`
- `src/service/uiaa/mod.rs`, `src/service/users/mod.rs`, `src/service/users/sso.rs`
- `src/service/deactivate/mod.rs`, `src/service/emergency/mod.rs`

Behavior:
- Adds an SSO-based UIAA fallback flow for SSO-origin users that works under
  strict CSP (server redirects + completion endpoints), binding the exact IdP
  into the UIAA session; persists UIAA session reverse lookups so fallback
  completion survives restart; repairs legacy SSO-origin metadata.
- Reactivates a deactivated local SSO account on re-login, but only when the
  account was self-deactivated (a persisted deactivation reason distinguishes
  self-service from administrative deactivation).
- Hardens the SSO grant-cookie path on both the set and removal cookies.

### 3) `auth/apple: native iOS Apple login exchange`
Files:
- `src/api/client/session/mod.rs`, `src/api/client/session/sso.rs`
- `src/api/client/session/sso/native_apple.rs`
- `src/api/router.rs`, `src/core/config/mod.rs`, `tuwunel-example.toml`

Behavior:
- Adds `POST /_matrix/client/unstable/org.mindroom.login/apple`.
- Verifies native Sign in with Apple identity tokens against Apple's JWKS,
  issuer, audience, expiration, and nonce (with a brief in-memory JWKS cache
  that refreshes on an unknown key ID).
- Accepts configured native app bundle IDs via
  `global.identity_provider.native_client_ids` while keeping the web Services ID
  valid; reuses the normal SSO mapping/registration/reactivation/loginToken
  path.

Note: the Apple `id_token` userinfo fallback that this fork originally carried
was merged upstream, so it is no longer a fork delta.

### 4) `config/rooms: default room power level override`
Files:
- `src/api/client/room/create.rs`, `src/api/client/room/create/mindroom_power_levels.rs`
- `src/core/config/mod.rs`, `src/core/config/check.rs`

Behavior:
- Adds a global room-creation config override for `m.room.power_levels`, applied
  before any per-request `power_level_content_override` (e.g. `users_default =
  50` for newly created rooms); validated as an object at startup.

## Operational Changes

### 5) `ci: fork release automation, container publishing, and GitHub checks`
Files:
- `.github/workflows/mindroom-release.yml`, `.github/workflows/auto-mindroom-release.yml`
- `.github/workflows/mindroom-container-release.yml`, `.github/workflows/mindroom-ci.yml`
- `scripts/fork_release_tag.py`, `docker/bake.sh`

Behavior:
- Computes `v<base_version>-mindroom.<n>` tags on `main`, creates/reuses the
  matching GitHub Release, publishes Linux `x86_64`/`aarch64` binaries, and
  dispatches container publication. Runs the fork's own GitHub-hosted checks.

## Tests

Fork integration tests live in the `mindroom-tests` crate
(`src/mindroom-tests/`), plus `default_test` database-path isolation in
`src/main/args.rs`. They pin the rebase-sensitive behaviors (SSO/UIAA, native
Apple, deactivation/erase, default power levels, edit-purge ↔ bundling
composition) so future rebases catch regressions.

## Runtime Configuration

### Edit compaction, purge, and bundling
```toml
[global]
mindroom_compact_edits_enabled = true
mindroom_edit_purge_enabled = true
mindroom_edit_purge_min_age_secs = 86400
mindroom_edit_purge_interval_secs = 3600
mindroom_edit_purge_batch_size = 1000
mindroom_edit_purge_scan_limit = 100000
mindroom_edit_purge_dry_run = false
# bundle_edit_relations defaults to true in the fork; set false only to opt out.
```

### Default room power levels
```toml
[global.default_power_level_content_override]
users_default = 50
```

### Native Sign in with Apple
```toml
[[global.identity_provider]]
brand = "AppleOIDC"
client_id = "chat.mindroom.matrix.apple"
native_client_ids = ["chat.mindroom.app"]
```

## Compatibility Notes
- Matrix event formats remain standard. With edit bundling on, served events
  (including `/sync`) may carry `unsigned.m.relations.m.replace` (the newest
  surviving edit, as a sync-shaped event without `room_id`); the fork's `/sync`
  compaction still delivers the surviving edit event itself.
- Superseded edits can be permanently removed when purge is enabled; the bundle
  compensates so history endpoints never serve stale pre-edit bodies.
- Admin-deactivated SSO accounts stay deactivated on future login attempts.
- The default power-level override only affects newly created rooms.
- Native Apple login requires the app bundle ID in `native_client_ids`.
