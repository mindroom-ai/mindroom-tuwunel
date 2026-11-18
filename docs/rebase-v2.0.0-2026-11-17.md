# Rebase on upstream v2.0.0 - 2026-11-17

## Inputs and scope

The starting fork main was `b364779152ceba2f2f159fa59df60e4f35d18091`
(`v1.9.4-mindroom.48`), 101 commits on upstream `main` at
`8c6c4a21054c0de5f4e70315870372def951e9ae`. The new base is exactly the
upstream `v2.0.0` release commit,
`a3feae1a26ec764ee88606c2c68b659a9b5fba8d` ("Bump 2.0.0.", 2026-11-10); its
annotated tag object is `37b1d7112`. `v2.0.0` is 121 commits past
`8c6c4a210`, which is its ancestor.

Fork main gained one commit while the rebase ran: #171 (`8923561c8`,
`v1.9.4-mindroom.49`, "sso: an external account is shown only the rooms it is
in or invited to"). It is replayed after #169, so the branch carries fork main
up to `8923561c8`. It applied without a conflict, and its diff differs from
the original in one line of context. The semantic reviews below were done
before the replay. Both reviewers then reviewed the replay and this record
and approved; what they found is corrected in this record's text, and one
email-pusher point is listed under "Points not changed here".

The old fork tip is preserved locally as
`backup/main-b36477915-before-v2.0.0-20261117`. No upstream commit or
published release tag is rewritten. The workspace version is now `2.0.0`, so
the release tooling previews `v2.0.0-mindroom.1`.

The result is 104 commits on `v2.0.0`: 100 of the 102 fork commits carried
over, #156 reduced to what upstream lacks, #131 dropped, two new commits for
delayed events, and this record.

## What upstream changed

The 121 commits add features rather than move code, so most fork commits
apply unchanged (66 of 101 are identical in `git range-diff`). The changes
that meet fork code:

- **Delayed events (MSC4140) in full** (`9ba670c30`, `a28cadd1d`, `6fee113e0`,
  `6554c6559`, `177f1c9cd`, `0d3014454`, `184190b93`, `630f8c75b`): any event
  can be delayed, scheduled events are stored and survive a restart, finished
  ones are kept for lookup and replay, with stable and unstable routes. This
  replaces the fork's #131.
- **Integrity checks** (`84fffa4bf`..`d7062f743`, `3fa47c092`, `a9da94f15`):
  earlier migrations become read-only checks with repairs an admin schedules.
- **Email notifications** (`564418fd3`, `c02c06c52`): a new pusher kind that
  reads the notification read stamps.
- **UIAA provider selection** (`c045fae4d`): the SSO stage binds to a provider
  the account's sessions link. This overlaps the fork's #156.
- **Member profile fields (MSC4466) and profile writes** (`261e9d1c9`..
  `1c9c46f5c`): account creation by an admin or MAS moved into
  `write_credentials` and `create_account`.
- **Sender-private unsigned fields** (`64549e2c8`, `6d0e05ac3`, `8946c21c4`)
  are stripped on every serving path and kept through redaction.
- **Server-signed bodies must be canonical JSON** (`fe793a77a`):
  `parse_json` takes the authentication scheme.
- **Cargo features** (`71c8ee1ca`, `240708d93`, `46cd00286`, `5711a47a4`):
  feature projection across the workspace crates; JWT login, email, S3 and GCS
  storage, Markdown and CPU pinning are optional.
- The dehydrated device routes became a directory module (`86fe3f64d`), and
  TURN credentials can come from a provider (`c13836ef3`).

## Outcome per fork commit

| Outcome | Commits |
| --- | --- |
| Dropped, owned by upstream | #131 |
| Reduced to what upstream lacks | #156 |
| Adapted, with a rebase note in the commit message | 27 (table below), and the CI owner |
| Carried with changed diff context only | native Apple owner, #110, #125, #142, #153, #171 |
| Carried unchanged | the other 66 |
| New in this rebase | the two delayed-event commits, this record |

### Dropped: #131, the delayed MatrixRTC call leave

Upstream took the fork's patch as `8ca7a893e` (PR #684) and then replaced it
with full MSC4140. The fork commit, its routes and its in-memory service are
dropped. Its last test was run against upstream's code to find what changed.
Everything it pinned still holds (restarts postpone the leave, it is sent once
they stop, another user's send is 404, cancel, send at once, an oversized
event, a suspended sender) except three policies:

1. Any event can be delayed, where the fork refused all but
   `org.matrix.msc3401.call.member` with 403.
2. A restart after the leave was sent answers 409 `M_UNKNOWN`, where the fork
   answered 404 `M_NOT_FOUND`.
3. A restart after a cancel answers 409 `M_UNKNOWN`, where the fork answered
   404.

The fork does not take upstream's policy for these. Both are restored as
small commits on top of upstream's implementation, described under "New
commits". Upstream's persistence, limits, finalised records, management
routes and advertised flags are kept.

### Reduced: #156, the linked provider for an untagged device

Upstream's `c045fae4d` owns the selection now (`select_idp`,
`Sessions::linked_provider`, `Providers::find_preferred_config`), with its
default and configuration-order choice. The fork commit keeps two rules
inside `linked_provider`:

- Only a finished sign-in links a provider (a session that holds `user_info`).
  A chained sign-in stores the user on the next session before that provider
  authenticates, and upstream counted it.
- A provider whose `client_id` a finished session records exactly is preferred.
  Only when no recorded ID is a configured `client_id` do the IDs resolve as
  brands, as upstream does. Without this, a retired ID equal to the default
  provider's brand hid the provider the account can still sign in at.

The fork test keeps its `linked-pending` and `linked-brand` rows; upstream's
`exercise_linked` fixture gains `user_info` on its sessions with unchanged
expectations.

### Adapted

Each is recorded in the owning commit's message under "Rebase note (v2.0.0)".

| Commit | Upstream change | Adaptation |
| --- | --- | --- |
| Edits owner | `a28cadd1d`: the `delayed_events` service and its configuration | The fork's module, service and `Default` entries sit beside upstream's |
| SSO owner | `c045fae4d` (`select_idp`), `46cd00286` (JWT behind `jwt_login`), `203430201`, `1c9c46f5c` | Upstream's `select_idp` is used unchanged. The fork keeps its origin gate, the JWT refusal for an SSO-origin account inside upstream's feature arm, and `bind_sso_identity_provider`. `select_bound_sso_idp` and its four unit tests go. `DeactivationReason` arguments sit beside upstream's boxed futures |
| Push owner, #108, #135, #140 | `c02c06c52`: `keyed_pushers`, `dispatch_push`, `queue_push`, `mailable` | The locked row and counter write stays in front of upstream's dispatch; #135 still removes #108's helpers, which leaves upstream's `append.rs` |
| #45, #143 | `64549e2c8`: infallible `without_private_unless_sender` | The visibility checks run in front of upstream's stripping; the title preview edit is stripped with `remove_private_unless_sender` |
| #61 | `6d0e05ac3`, `203430201` shortened `load_left_room` | The `too_many_lines` expectation goes |
| #84 | `86fe3f64d`: dehydrated device directory module | The size check moves to `put_dehydrated_device.rs` |
| #88, #95, #97, #103 | Join extras tests and MSC4466 checks in the join path | Import and test unions |
| #90 | `261e9d1c9` | Import unions in the room summary and profile service |
| #96, #99, #124 | `fe793a77a`: `parse_json(&request, route, scheme)` | The origin key is resolved and stored before the scheme-aware parse; `MasAuthenticated` is exported beside upstream's `jwt_login`-gated `validate_user` |
| #105 | `6fee113e0` | The audience flag follows upstream's delayed-event flags in `/versions` |
| #123 | `8946c21c4`, `84fffa4bf` | Import union; `SUCCESSOR_STEPS` follows upstream's `Restoration` |
| #129 | `3fa47c092`, `a9da94f15`, `203430201` | The fork's replacement of the `fetch` closure stays; the walk constants follow upstream's `Checked` type |
| #130 | `6fee113e0` moved the send routes into delayed-event wrappers | The ephemeral route leads the router chain |
| #138 | `1c9c46f5c`: `write_credentials`, `create_account` | Both call the fork's `create_new`, so an account created meanwhile is still refused; a refused profile value still returns before the account exists |
| #143 | `a9da94f15`: `recount_summary` | The fork's comment moves to that function |
| #147 | `564418fd3`: `notification_read` | The fork's read computation stays for `/notifications`; upstream's function is unchanged |
| #162 | `c13836ef3`, `0be2678cd` | Import union; the external mark precedes upstream's renamed display name suffix |
| #163 | `71c8ee1ca`, `240708d93`: a `[features]` table in the admin crate | The fork's `client` and `server` features are merged in. The invite routes and their module are gated on `server`, see "Verification" |
| #169 | `501bbd92b` rescored five MSC rows | Counts recomputed: 290 `yes`, 62 `part`, 446 `no`, 312 `n/a`; the delayed-event scope then moves MSC4140 to `part` |
| CI owner | `e754d6cdd` | The formatter pin moves, see "Formatter" |

One upstream test expectation changes with the SSO owner:
`uiaa_sso_binds_a_linked_provider` (`c045fae4d`), case `password`, expected an
account that signs in with a password to be offered an SSO stage bound to a
linked provider. The fork offers such an account its password stage only (the
SSO-origin gate, pinned by `uiaa_sso_policy.rs`), so the case expects none.

### New commits

**`delayed_events: answer M_NOT_FOUND on the unstable routes for a finished
delay`.** matrix-js-sdk 41.7.0, which MindRoom Chat pins, schedules a
replacement delayed leave when a restart or cancel answers `M_NOT_FOUND`, and
does not handle 409 `M_UNKNOWN`. On the unstable `org.matrix.msc4140` paths,
in the path form and the earlier body form, an action that conflicts with a
finished event answers 404 `M_NOT_FOUND`. The stable path keeps upstream's
409, and the service is untouched. Upstream's boot test drives the unstable
path, so two of its expectations move from 409 to 404.

**`delayed_events: only the sender's own MatrixRTC call leave may be
delayed`.** A client can delay only an `org.matrix.msc3401.call.member` state
event with empty content, under a state key in its own call slot (the user ID
and an underscore, with or without one more underscore in front). Everything
else is refused with 403 `M_FORBIDDEN` before anything is read. This limits
what can pass through four gaps the review found in delaying arbitrary events
(below). It does not patch the four upstream paths, and the gaps themselves
stay. Upstream's service is not held to the scope, so
three upstream boot cases that need an event the routes now refuse schedule
it in the service; the other cases delay a call leave and keep what they
check. The MSC4140 row of the compliance table moves from `yes` to `part`.

## Rulings

Decisions taken during this rebase. The fork's owner ruled on 3 and 4. For 1,
2 and 6 both reviewers recommended the outcome and he did not overrule it. 5
was put to him as the default; he did not object and did not rule on it.

1. #156 keeps its finished-sign-in filter on upstream's selection.
2. The SSO-origin gate stays, and upstream's `password` case expects no SSO
   stage.
3. A conflict with a finished delayed event answers 404 on the unstable paths
   only.
4. Delayed events are limited again, to the sender's own call leave: first to
   the call membership type, then, after review, to empty content in the
   sender's own slot.
5. A delayed `m.call.member` leave stays accepted. ruma reads that name as an
   alias of `org.matrix.msc3401.call.member` and writes the latter, so the
   event that is stored and sent is the allowed one.
6. Email notifications and the fork's read state are a separate follow-up and
   do not block this rebase (see "Points not changed here").

## Formatter

`nightly-2026-11-03` passes unmodified `v2.0.0`; upstream's `e754d6cdd` names
that nightly. The locked Fenix formatter (rustfmt 1.10.0-nightly `1ed2df61a1`
2026-08-04) reports fourteen files there, and `nightly-2026-10-04`, the
previous CI pin, reports one (`src/core/alloc/je.rs`). The CI pin moves to
`nightly-2026-11-03`; its comment and the CI commit name `v2.0.0`.
`flake.lock` is unchanged from upstream.

## Semantic review

The implementer was Claude Fable 5.2. Reviews were read-only and by other
models: GPT-6.2 Sol and GPT-6.2 Astra.

**Integrity checks against fork-written rows** (Sol, before the code review).
The fork changes nothing under `src/service/integrity`,
`src/service/migrations` or `src/admin/integrity`, and upstream's 21 migration
markers are the 21 the old fork wrote, so a database the old fork upgraded
reruns none of them. No new destructive pass runs by default. Three advisories
stay open: the `relation-index` check reports `relatesto_typed` rows the edit
purge leaves behind; the `short-ids` check labels a live room whose thread
activity pointer holds a backfilled count as purge residue, and its repair
skips such rooms; and with `media.prune_missing = true` the media startup
check deletes metadata of provider-backed media, which predates this rebase.

**Drops** (Sol) and **adaptations** (Astra), on the 100 replayed commits.

- Sol found four gaps in delaying arbitrary events, none of which the fork's
  call-leave-only #131 had. The send path rechecks suspension alone, so a
  locked account's delayed event is still sent; a scheduling request that
  authenticated before a deactivation can store its event after the
  deactivation swept the account's events; the state route's checks on
  invites and canonical aliases run at scheduling only; and the record of a
  sent event serves its original content after a redaction or purge. The
  scope commit closes none of them in upstream's code: the send path still
  rechecks suspension alone, and scheduling is not serialized with a
  deactivation. It limits what can pass through them to the sender's own
  empty call leave, which takes one of its own devices out of a call and
  whose record holds `{}`.
- Astra found that #156, as first reduced, let a retired ID equal to the
  default provider's brand hide the provider the account can sign in at. The
  exact-ID rule above fixes it, with the `linked-brand` test row back.
- Both found the `FORK_CHANGES.md` text for #156 stale; it is rewritten, with
  `docs/authentication/providers.md`.

**The three fix commits** (#156, the 404 mapping, the scope), reviewed by both.
The first scope allowed any call membership. Both reviewers found that a
delayed call join, or any content under that type, would still be sent after
a lock and read back from its record, and Astra that room auth does not tie a
state key with an underscore in front to its sender, so a delayed leave could
empty another user's slot. The scope became the sender's own empty leave. Both
then approved; their two documentation notes (what matrix-js-sdk does on other
errors, and the MSC3779 room versions) are fixed in the commits and were not
reviewed again.

Points not changed here:

- Email notifications (`c02c06c52`) read the notification read stamps
  (`564418fd3`). After a receipt for an older event those stamps hold the
  receipt's arrival, so the email pusher can treat later, unread events as
  read and never mail them, and the same stamp can reset its throttle. The
  fork's `/notifications` (#140, #147, #153) no longer trusts the stamps. The
  email preview also renders an edit's fallback body, not the finished reply.
  The email pusher also checks that the recipient is joined or invited when
  it scans, not when it reads the room's current name and alias for the
  digest: if the membership or invite is revoked in between and the room is
  then renamed, the mail can carry the new name or alias. That needs a
  notification queued while the user was in the room and that race; it lets
  no outsider in (Sol, in the review of the #171 replay). Email is compiled
  by default (`email` is in the default features of `src/main/Cargo.toml`);
  delivery needs `[global.smtp]`. No code is changed and no issue was filed as
  part of this rebase.
- A delayed event from a database that already ran upstream's MSC4140 without
  the scope would still be sent. The old fork held delays in memory, so a
  fork database has none.
- The three integrity advisories above.

## Verification

Checks ran on a second machine with the project's Rust 1.95.0 inside the
flake's dynamic development shell, rustfmt nightly-2026-11-03,
`TUWUNEL_DATABASE_PATH` unset inside the shell, and
`LD_LIBRARY_PATH` naming the shell's RocksDB and libstdc++. Builds and tests
ran in a private user and network namespace with only loopback.

**Every commit.** `cargo fmt --all -- --check` and
`cargo clippy --offline --locked --workspace --all-targets --all-features -- -D warnings`
passed on each of the 103 commit trees before this record, each on its own
tree; trees that a later fold changed were checked again. This record changes
documentation only.

**Full suite** (`cargo test --offline --locked --workspace --all-targets
--all-features --no-fail-fast`). The counts include the child processes some
boot tests start. The upstream count sums the result lines. In the fork run
two pairs of child result lines interleaved, so its count is derived: 2513
tests started (the sum of the `running N tests` lines) minus 4 ignored, with
exit status 0, no `FAILED` line and no failed target.

| Tree | Passed | Failed | Ignored |
| --- | ---: | ---: | ---: |
| Unmodified upstream `v2.0.0` | 2208 | 0 | 4 |
| Rebased fork (code of the final tip, `fa98dfc6c`) | 2509 | 0 | 4 |

`fa98dfc6c` is the scope commit as it was when the suite, the upgrade probe
and the release build ran. Its message and its `FORK_CHANGES.md` entry were
reworded afterwards; no source file differs.

By test name, upstream lists 1959 tests in 162 targets and the fork 2244 in
218. No upstream target is missing. Six upstream unit tests are replaced by
fork tests, as before this rebase: the four
`decode_apple_userinfo_from_id_token_*` (native Apple owner) and the two
`grant_session_*cookie_path*` (#161).

The fork run is confined to eight CPUs (`taskset`, with jemalloc's
`percpu_arena` disabled). The reason was measured before #171, when
`sso_registration_rules` had 43 tests: on all 96 CPUs of that machine, and on
24, it failed a few of them at random on the test's own ten-second deadlines;
the old fork main on the old base failed the same way, and the target passed
43 of 43 on eight CPUs or with six test threads. With #171 it has 47 tests.
They passed in the eight-CPU full run; the runs on more CPUs and with six
test threads were not repeated.
Each test boots a server whose pools are sized from the visible CPUs; that is
the suspected cause and was not measured.

**Known flake.** Upstream's
`rooms::timeline::tests::accepting_an_invite_installs_state_before_publishing_the_join`
failed one of eight runs of `tuwunel_service --lib` on eight CPUs, on the
tree before the delayed-event commits, and once in a full run. It is not
changed here.

**Environment.** The development shell exports `TUWUNEL_DATABASE_PATH`, which
overrides the database path that fifteen fork unit tests configure (twelve
`edit_purge`, three `users::sso`), so they share one database and fail. They
pass with the variable unset inside the shell. No patch is carried.

**No default features.**
`cargo check --offline --locked --workspace --no-default-features` passes on
`v2.0.0` and failed on the first replayed tip: #163 defines its own request
types, whose server half exists only under the admin crate's `server`
feature, and a workspace check without default features builds that crate
alone. The old fork had the same property. The invite routes and their module
are now gated on `server`, which every dependent enables, and the check
passes on the final code.

**Dropped tests against upstream's code.** The #131 test, with only its three
policy expectations changed, passes on upstream's MSC4140 (see "Dropped").
The #156 test differed in two of six cases, `linked-pending` and
`linked-brand`; both pass again with the reduced commit.

**Before and after.** Each new or reduced rule has a test that fails without
it, run on throwaway commits: the 404 mapping (409 against 404), the exact
provider ID (the default bound instead of the linked provider), and the scope
with the old type-only rule, without the slot check, and without the content
check (200 against 403 each).

**Upgrade probe.** A release build of the old fork main
(`v1.9.4-mindroom.48`, `b36477915`) seeded a fresh database: three users, a
private room with a name and topic, a message with three edits, a thread root
with three replies of which one is redacted, uploaded device identity keys, a
display name, a 16 KiB media upload, a MatrixRTC call membership with a
scheduled delayed leave, and a self-deactivated user. It restarted with the
edit purge enabled until the two superseded edits were purged. The release
build of the rebased fork (`fa98dfc6c`) then served a copy of that database,
was restarted, and passed all 60 checks:

- Before the restart, 46: the pre-upgrade tokens and devices, password login,
  the deactivated user refused, the display name, joined members, room name,
  topic and full state, the purged edits still gone and the newest one
  bundled, the thread count, latest reply and relations, the redacted reply,
  `/messages`, initial `/sync` and incremental `/sync` from the pre-upgrade
  positions, identity keys kept with an exact-key retry accepted and a
  replacement refused, and the media download byte-identical and refused
  without a token. The call membership is still there, and the old `delay_id`
  answers `M_NOT_FOUND`, since the old fork held delays in memory; that is
  the answer after which a client schedules a replacement. A delayed call
  join, a leave in another user's slot, a topic and a message are refused
  with 403. The user's own delayed leave is scheduled, listed, restarted and
  sent; a restart after that answers 404 on the unstable path and 409 on the
  stable one. New messages, an edit and a thread reply arrive on the
  pre-upgrade sync position.
- After the restart, 14: a second scheduled leave survived it, and the same
  answers for tokens, purged edits, the bundled edit, members, identity keys,
  media, the thread and sync.

Neither start ran a migration beyond loading schema version 17. No server
panicked or logged an error. The configuration file used the old top-level
keys (`allow_registration`, `registration_token`, `allow_federation`), and
none of its keys was reported unknown. Both versions warn about one unknown
parameter, `version_extra`, which is the development shell's
`TUWUNEL_VERSION_EXTRA` variable. The probe scripts are not part of the tree.

The seed is from `.48`, one commit before #171, and holds no external
account. That a `.49` database upgrades the same way is a conclusion from
#171's source, not a probe result: #171 adds no column, migration or
configuration key, and reads the `userid_external` mark and room membership
that `.48` already writes. The probe did not carry an external account or a
stored SSO mark through the upgrade; #171's four test cases pass in the full
suite, on fresh databases.

**Release build.** `cargo build --offline --locked --release --bin tuwunel`
succeeded on the final code (binary sha256 `57d5f22afde0…`), and the upgrade
probe ran with it.

**Release tooling.** `scripts/fork_release_tag.py` previews
`v2.0.0-mindroom.1` and moves no tag.

Not verified: a real MatrixRTC client or call against this build; live
federation; a real identity-provider exchange; a production database snapshot
or production-scale load; an external account carried through the upgrade;
cross-architecture packaging; a remote CI run. The
matrix-rust-sdk claim (it delays only what a widget asks) was read from its
`main` branch, not a pinned release. The `50/80` score of the MSC4140
compliance row is an estimate. Each full suite ran once per tip.

## Next rebase

Follow the earlier procedure (replay first, gate after, fix forward into the
owners), and also:

1. Run each dropped fork test against upstream's replacement before deciding
   that upstream owns the change. The #131 test found three policy
   differences in minutes, and two of them would have broken calls.
2. When upstream widens a feature the fork had limited on purpose, decide the
   scope first. Restricting the client routes to what the pinned clients send
   replaced four separate fixes, and it took three review rounds only because
   the first rule checked the type and not what the event does.
3. Check what the pinned clients do with each changed status code, in the
   SDK's source at the pinned version.
4. Confine the full suite to a few CPUs on a many-core machine, and unset
   `TUWUNEL_DATABASE_PATH` inside the development shell.
5. Run `cargo check --workspace --no-default-features` on the tip; no earlier
   rebase did, and it found a fork-caused break.
6. Fetch fork main again before the documentation commit. It moved during
   this rebase.
