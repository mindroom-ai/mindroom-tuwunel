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

#### 5) `config/rooms: add default room power level override`
Files:
- `src/api/client/room/create.rs`
- `src/core/config/check.rs`
- `src/core/config/mod.rs`

Behavior:
- Adds a global room-creation config override for `m.room.power_levels`.
- Applies the server default before any per-request `createRoom`
  `power_level_content_override`.
- Supports defaults such as `users_default = 50` for newly created rooms.
- Validates at startup that the override is an object/table-shaped value.

#### 6) `auth/apple: add native iOS Apple login exchange`
Files:
- `src/api/client/session/mod.rs`
- `src/api/client/session/sso.rs`
- `src/api/router.rs`
- `src/core/config/mod.rs`
- `tuwunel-example.toml`

Behavior:
- Adds `POST /_matrix/client/unstable/org.mindroom.login/apple`.
- Verifies native Sign in with Apple identity tokens against Apple's JWKS,
  issuer, audience, expiration, and nonce.
- Rejects native Apple tokens that include a nonce when the request omits the
  raw nonce, and rejects requests that supply a nonce when the token omits one.
- Caches Apple's JWKS briefly in memory so repeated native Apple logins do not
  fetch signing keys on every request, while refreshing once when a cached set
  does not contain the token's key ID.
- Accepts configured native app bundle IDs through
  `global.identity_provider.native_client_ids` while preserving the existing
  web Services ID as a valid audience.
- Selects the native Apple provider from an explicit `providerId`, or from the
  single configured AppleOIDC provider when there is exactly one.
- Reuses the normal SSO account mapping, registration, reactivation, and
  Matrix `loginToken` creation path.

#### 7) `mindroom/edits: serve bundled m.replace aggregations on originals`
Files:
- `src/api/client/context.rs`
- `src/api/client/message.rs`
- `src/api/client/relations.rs`
- `src/api/client/room/event.rs`
- `src/api/client/search.rs`
- `src/api/client/threads.rs`
- `src/core/matrix/pdu/tests.rs`
- `src/core/matrix/pdu/unsigned.rs`
- `src/mindroom-tests/tests/bundled_edit_aggregations.rs`
- `src/service/rooms/pdu_metadata/mod.rs`

Behavior:
- Serves the latest same-sender `m.replace` edit as a bundled aggregation at
  `unsigned.m.relations.m.replace` on originals returned by `/messages`,
  `/context`, `/relations` (including `recurse`), `/event`, `/threads`, and
  `/search` (MSC2675/MSC2676).
- The bundle is the full replacement event in client format, including
  `room_id` and `origin_server_ts`, so clients can hydrate the final content
  even when the edit event itself falls outside a pagination window.
- Selects the same edit the purge keeps (newest by PDU stream order per
  target and sender) and skips relation-index entries whose edit PDU was
  purged, falling through to the newest surviving edit.
- `/sync` is unchanged: sync compaction already delivers the surviving edit
  event in the timeline, so no bundle is added there.

### Operational Changes

#### 8) `ci: add GitHub release workflow for ARM and x86_64 binaries`
Files:
- `.github/workflows/mindroom-release.yml`

Behavior:
- Adds tagged binary publishing for Linux `x86_64` and `aarch64`.

#### 9) `ci(release): auto-tag main pushes and create releases`
Files:
- `.github/workflows/auto-mindroom-release.yml`
- `scripts/fork_release_tag.py`

Behavior:
- Computes `v<base_version>-mindroom.<n>` tags on `main`.
- Creates or reuses the corresponding GitHub Release.

#### 10) `ci(container): publish release containers`
Files:
- `.github/workflows/auto-mindroom-release.yml`
- `.github/workflows/mindroom-container-release.yml`
- `docker/bake.sh`

Behavior:
- Dispatches container publication for MindRoom release tags.
- Uses the configured buildx builder for release container builds.

#### 11) `docs: summarize fork runtime and release additions`
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

## Recommended Rollout
1. Enable `mindroom_compact_edits_enabled` first.
2. Enable purge in dry-run mode with `mindroom_edit_purge_dry_run = true`.
3. Inspect logs for candidate volume and purge cadence.
4. Disable dry-run when behavior is confirmed.

## Behavior Summary
- Edit lifecycle changes reduce redundant edit traffic in `/sync` and can
  reclaim storage by purging superseded historical edits.
- History endpoints bundle the latest same-sender `m.replace` edit onto served
  originals, so purged or out-of-window edits cannot leave clients stuck on
  the pre-edit body.
- Apple OAuth fallback improves sign-in robustness when `userinfo` is
  unavailable.
- Native iOS Apple login can exchange a signed app-bundle ID token for the same
  short-lived Matrix login token used by browser SSO.
- UIAA SSO fallback supports strict-CSP deployments that cannot rely on inline
  browser logic in the default flow.
- Returning SSO users are reactivated only when they self-deactivated.
- Newly created rooms can inherit a homeserver-wide default power-level
  override.

## Operational Summary
- Main-branch pushes can auto-create MindRoom release tags and GitHub Releases.
- Tagged releases publish Linux binaries for `x86_64` and `aarch64`.
- Release tags can also trigger container publication.

## Current Status
- PR #3 native Apple review follow-up has been addressed.
- `refresh_apple_jwks` rechecks the write-locked JWKS cache for the requested
  Apple key ID before fetching, so concurrent unknown-key refreshes can reuse a
  cache update from another request.
- Apple JWKS key lookup uses the key ID already validated by
  `apple_id_token_header`; missing-key-ID auth errors remain at header
  validation.
- Verification completed for this follow-up: focused SSO tests, the requested
  API/core/service cargo check, and `git diff --check`.

## Compatibility Notes
- Matrix event formats remain standard.
- Clients may observe fewer intermediate edit events in `/sync` when compact
  mode is enabled.
- Superseded edits can be permanently removed when purge is enabled; history
  endpoints compensate by serving the kept edit as a bundled `m.replace`
  aggregation on the original.
- Admin-deactivated SSO accounts stay deactivated on future login attempts.
- The default power-level override only affects newly created rooms.
- Native Apple login requires the app bundle ID to be listed in
  `native_client_ids` for the Apple provider.
