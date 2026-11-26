# MindRoom Tuwunel Fork - Changes vs Upstream

This document describes the current MindRoom fork behavior on top of upstream
`tuwunel`. The fork is rebased directly onto upstream commits; see
`docs/rebase-*.md` for the per-rebase log. As of the 2026-11-25 rebase the base
is the upstream `v2.1.0` release (`4b0d90d97`), 3 commits past the previous
base, upstream `main` at `f7dc0770b`. Earlier changes merged upstream and
dropped from the fork include the room-creation default power-level override
and the SSO grant-cookie path hardening (2026-07-07 rebase; the cookie's name
and path now differ again), and the drained/zero one-time-key counts fix and
public MatrixRTC transport discovery (2026-07-21 rebase, upstream
`164b8da61`/`007033cd5` and `a1776c368`). The Sliding Sync `num_live`
correction was dropped at v1.9.0 because upstream now contains the same
implementation and tests (`6acc3a7d1`, `942dd66e3`, and `a508c7331`).

At v1.9.1, test database isolation, `/messages` pagination bounds and token
validation, and quiet-room full-state sync are supplied by upstream. Their
fork implementations and duplicate fixtures are dropped. Device-key read-error
handling and one-time-key cleanup also reuse upstream's implementations; the
remaining fork policy is immutable device identities with atomic upload/removal.
See the [v1.9.1 rebase record](docs/rebase-v1.9.1-2026-09-12.md).

At v1.9.3, optional appservice user discovery
(`show_appservice_users_in_user_directory`) is upstream with the same option
name and default (PR #594, `b996bc597` and `ef274db22`), so the fork commit and
its duplicate tests are dropped. Upstream now removes a device's identity-key
row with the device (`1347197ee`) and refuses a device ID that names a
cross-signing key (`99c6c320a`); the fork keeps only its per-device lock and
immutability policy around them. UIAA password flows follow upstream's
credential-matched rule (`0726625e6`). The upstream fixes the fork had
backported from `main` and that `v1.9.3` does not contain were carried as
individual cherry-picks. The fork's sliding sync profiles fix (#60) was
dropped at that rebase because its test passes on v1.9.3 without it, but
upstream's rewrite of the profiles extension (`005830a1b`) only checks that
each user shares a joined room with the requester, so the fix was carried
again (#76) until v1.9.4.
See the [v1.9.3 rebase record](docs/rebase-v1.9.3-2026-10-05.md).

On upstream `main` (`3f5db6d3a`), the fork's thread summary fixes (#19-#21)
are upstream: PRs #617-#619 (`c9307347a`, `787099ab5`, `e6735fdf3`) and their
rework into a single `rebuild_thread_summaries` startup pass (`b0c52a081`),
which replaces the fork's `recount_thread_replies` and
`scrub_redacted_thread_latest` passes and runs once on a fork database. The
bound on the search for a redacted latest reply's replacement (#24) stayed in
the fork, on upstream's rewritten helpers, until v1.9.4. All eight temporary
backports are in `main` and are no longer carried. Account deactivation now
holds upstream's admin-room lock and refuses the last admin (`3fe9ef681`,
`5420f2e56`); the fork records its deactivation reason under that lock.
See the [upstream main rebase record](docs/rebase-upstream-main-2026-10-06.md).

At v1.9.4, upstream contains 34 of the fork's fixes: half through the fork's
own upstream changes (PRs #624-#637 and three directly applied commits), some
extended by the maintainer, and half as the maintainer's own versions of the
same fixes. Their fork commits are dropped with their tests and entries, and
upstream's versions are kept where they differ. Among them: the thread reply
search bound (#24, now `thread_latest_reply_search_limit`), the UIAA body bound
(#27), the sliding sync timeline cap and knock timeline (#29, #35), federated
key query and claim filtering (#30, #58), knock and leave membership fixes
(#31-#33, #46, #75), the SSO login-token confirmation, provider chaining and
username fallback (#34, #37, #62), redirect and IP denylist hardening (#41,
#51, #63, #65, #83), read receipt visibility (#49, #102), hierarchy cache
authority (#52, #72), remote profile limits and sliding sync profile rooms
(#76-#78, #86), and the prev-event room and old-event cutoff checks (#47, #48).
Nine commits keep only what upstream lacks: #57 (stored-copy guard and
redaction of send_join events), #66 (the federated receipt path), #67
(`/state_ids` and local-build room checks), #84 (uploaded key sizes), #87 and
#103 (on upstream's knock member skip), #88 (test only), #89 and #90 (on
upstream's rewritten sliding sync fold and profile merge). The legacy SSO
callback now uses upstream's Continue page, which trusts only the
`well_known.client` origin and `oidc_registration_allowed_redirect_hosts`; the
fork's `sso_trusted_redirect_hosts` option is gone.
See the [v1.9.4 rebase record](docs/rebase-v1.9.4-2026-10-11.md).

On upstream `main` (`8c6c4a210`), upstream contains 13 more of the fork's
fixes: the former-member state, `/context` and summary reads (#26, #53, #70,
#71) and the alias listing the fork carried as a backport (#113), the
send_join and knock storage checks (#57, #74, #87), refresh-token rotation
under the device lock (#54, #85, upstream PR #629), the sliding sync fallback
key wakeups (#106), same-millisecond room ids (#107) and join positions across
profile updates (#144). Their commits, tests and entries are dropped, and
upstream's policy is kept where it differs. Upstream also moved the Matrix
types into the `tuwunel_matrix` crate, split the API into one crate per
surface (`src/api/server` is now `src/api/federation`), made `/sync` v3 a
directory module and grouped the configuration into sections. Moved options
keep their old names as legacy keys; the fork's `mindroom_*` options stay
top-level, and `bundle_edit_relations` is now `rooms.relations.bundle_edits`,
still on by default in the fork.
See the [upstream main rebase record](docs/rebase-upstream-main-2026-11-08.md).

At v2.0.0, upstream implements delayed events (MSC4140) in full: any event can
be delayed, scheduled events are stored and survive a restart, and finished
ones are kept for lookup. The fork's delayed MatrixRTC call leave (#131), which
upstream first took as it was, is dropped with its routes, in-memory service
and entry. Two of its policies stay, as small commits on upstream's
implementation: a client can delay only its own call leave, and on the
unstable paths an action that conflicts with a finished delayed event answers
404, which MatrixRTC clients rely on. Upstream also binds the UIAA SSO stage
to a provider the account's sessions link (`c045fae4d`), so #156 keeps only
what upstream lacks: a provider is linked by a finished sign-in, and one whose
ID a sign-in records exactly is preferred to one matched by brand. The other
fork commits are carried, 27 of them and the CI owner adapted to upstream's
changes: the scheme-aware `parse_json`, the notification dispatch helpers of
the new email pusher, account creation in `write_credentials` and
`create_account`, sender-private unsigned fields, the admin crate's feature
table, and the CI formatter pin.
See the [v2.0.0 rebase record](docs/rebase-v2.0.0-2026-11-17.md).

On upstream `main` (`f7dc0770b`), upstream contains 18 more of the fork's fixes
(11 via merged upstream PRs, 5 via upstream's own replacements, 1 test helper
fix, and 1 backport): `/sync` history visibility (#55), never-joined left-room
state (#61), `/state_ids` and local-build room checks (#67), sliding sync
selector fingerprinting (#68), unfolded `/initialSync` and `/notifications`
(#69), `send_join` create room-id test and event authorization (#88, #95),
unified `provenance.rs` local-join and create checks (#100, #116), SSO
exclusive-namespace username checks (#111), `/state_ids` deduplication (#112),
`send_join` retry membership and restricted-join authoriser checks (#114,
#120), refresh-token access refusal (#127), redacted events excluded from
`/relations` (#141), UIAA linked-provider selection (#156), watch-probe
polling, and active local key counting (#188). Four commits keep only what
upstream lacks: #45 (edit-bundle visibility), #119 (sliding sync range
deduplication), #129 (walk budgets, dual-parent dedup, and backfilled `from`
normalization on upstream's 3-level recursive `/relations`), and #183 (stripping
stored `m.relations` on embedded `m.replace` edits and `#143` previews using
upstream's `served_unfolded_to`/`remove_aggregations`). Upstream also adds
MSC4354 sticky events (`443a6dd30`), moves the default SSO callback path to
`/_tuwunel/sso/callback/{idp_id}` (`6e7510b88`), requires auth on
`/rtc/transports` (`d6af5010a`), and updates the locked toolchain to Rust
1.100; the fork's delayed-event scope now also permits a sender's own delayed
MSC4354 sticky `m.rtc.member`/`org.matrix.msc4143.rtc.member` leave, sliding
sync limits now cover `sticky_events`, and the CI formatter pin moves to
`nightly-2026-11-20`.
See the [upstream main `f7dc0770b` rebase record](docs/rebase-upstream-f7dc077-2026-11-24.md).

At v2.1.0 (`4b0d90d97`), upstream adds three commits past `f7dc0770b`: a
database pool worker-group fix for hosts whose storage queue IDs exceed the
highest available core ID (`986f447cd`), a Playwright Dockerfile tweak
(`0c2ff3441`), and the `2.1.0` version bump (`4b0d90d97`). All 106 fork commits
(through `#192`) replay unchanged with no conflicts, no drops, and no code
adaptations; the CI formatter pin stays `nightly-2026-11-20`.
See the [v2.1.0 rebase record](docs/rebase-v2.1.0-2026-11-25.md).

## How To Inspect
- Fork commits: `git log --reverse --oneline <upstream-base>..HEAD`
- Files changed in the fork: `git diff --stat <upstream-base>..HEAD`
- Per-commit patch: `git show <sha>`

Keep the delta limited to fork behavior and required compatibility changes.
Leave unrelated upstream tests and tooling unchanged; handle host-specific
verification needs in the test environment instead of carrying extra patches.

The history keeps the v1.9.1 ownership layout of eight commits (shared test
infrastructure, five runtime features with their tests and compatibility
changes, CI, and docs), followed by the squash-merged fork PRs that upstream
does not contain, in merge order (#15 to #192), with the earlier rebase records
in their historical positions, the two delayed-event commits and v2.0.0 record
after #171, the `f7dc0770b` record and Rust 1.95 recursion-limit commits before
#192, and this rebase's documentation last. No upstream backports are
carried. The [v2.1.0 rebase record](docs/rebase-v2.1.0-2026-11-25.md)
describes this rebase. Earlier rebase records remain historical snapshots.

## Runtime Changes

### Stored signing keys of a remote server stay small

Every signing-key document fetched for a server, from the server itself or
from a notary, was merged into the one row stored for that server, and nothing
removed keys from it. A server that answered each fetch with new key ids grew
that row without bound, and every key lookup for the server parsed the whole
row, including the lookup for each incoming federation request it signs. A
merge that would make the row larger than 64 KiB of serialized JSON now keeps
the fetched keys and the stored keys the running acquisition asked for, those
it fetched and those it found cached, so two notary documents of one batch,
each with a requested retired key, keep both (#174). Retention is bounded by
total serialized size, not by a number of keys: if the fetched keys and the
requested ones are still over 64 KiB, only the fetched keys are kept, which can
drop a requested key, and a fetched document over 64 KiB on its own is not
stored. A batch acquisition counts a key as found only when the stored row
holds it after each write, so a key dropped this way is missing again and is
asked for from the next source. (A single lookup that fetches a key to verify
one signature still uses the key it fetched even when the row could not keep
it.) A stored row over 64 KiB, left from before
this change, is treated as absent: lookups do not parse it, and the next fetch
for the server replaces it. Servers publish a few keys in a few hundred bytes.
Upstream has the same bug. Files:
`src/service/server_keys/{mod.rs,acquire.rs,get.rs}`; tests in
`src/service/server_keys/tests.rs` and
`src/main/tests/federation_notary_key_batch.rs`.

### Lookups on a named server read small answers

A room summary, a remote alias lookup and a remote profile lookup, which a
client can make without an account, and the signing-key fetch for the origin
of an incoming federation request read the answer of the server the request
names under the generic federation response limit (256 MiB by default). Those
answers are small (the summary of a large space can reach several megabytes),
but a slow answer kept up to 256 MiB buffered per request until the read timed
out, and a large one was decoded in full before anything checked it. A room
summary also asked every server in its `via` list, even one named many times,
32 at a time. The room summary now asks each of the first 8 distinct servers
once and reads at most 8 MiB from each, alias answers and signing-key
documents (from the origin or from a notary for one server) are read up to
1 MiB, and profiles up to four times the 64 KiB profile limit.
A larger answer is dropped while it is read, and no limit exceeds
`network.http.max_response_size`. Upstream has the same bug. Files:
`src/service/federation/{execute.rs,feds.rs}`,
`src/api/client/room/summary.rs`, `src/service/rooms/alias/mod.rs`,
`src/service/server_keys/request.rs`, `src/service/profile/{mod.rs,remote.rs}`;
test in `src/main/tests/federation_lookup_response_limits.rs`.

### Redacting a message redacts its edits

A redaction pruned only its target, so the target's `m.replace` edits kept the
new text in `m.new_content` and stayed in the search index. Any member could
still read a deleted message's latest text by the edit's id, from the target's
relations, in `/messages` or through `/search`. A redaction now also redacts
the target's edits that the edit bundle counts (same sender and type), with the
same redaction as their reason. Upstream redacts only the target too. Files:
`src/service/rooms/{timeline/append.rs,pdu_metadata/bundling.rs}`; test in
`src/main/tests/redaction_edits.rs`.

### Redacting replaced state redacts the copy in its successor

A state event stores the content of the state it replaces in
`unsigned.prev_content`. A redaction pruned only its target, so when the target
was a state event that had since been replaced, the event that replaced it kept
the removed content. Any member who could see that event, also one who joined
after the redaction, could still read it in `/messages`, `/context`, `/event`
or `/state`. A redaction of a replaced state event now also sets its
successor's `prev_content` to the redacted content, in the same write as the
target. The target is looked for at most 16 replaced states back from its
key's current state, and not when it was already redacted, so a copy left by a
redaction made before this change is not repaired. The successor of a state
event further back, or one on the losing side of concurrent changes to the
same key, keeps its copy. Upstream has the same bug. Files:
`src/service/rooms/timeline/{append.rs,mod.rs,redact.rs}`; tests in
`src/main/tests/redaction_prev_content.rs`.

### Sliding sync refuses oversized selections

A sliding sync connection keeps the lists, room subscriptions and extension
filters a request names for later requests, and each pass matches every room
in the window against all of them: each `required_state` selector costs a
lookup and a stored fingerprint per room, and each `spaces` entry a state
read. None of these had a length limit, so one request could keep a worker
busy for minutes. A request is now refused with 400 `M_INVALID_PARAM` when it
names more than 64 lists, a list ID longer than 64 bytes (the limit Synapse
documents), more than 16 distinct ranges in a list, more than 16 distinct
entries in a list's `spaces`, `tags`, `not_tags`, `room_types` or
`not_room_types`, more than 256 `required_state` selectors in a list or
subscription, more than 256 distinct selectors across the lists one room can
match, or with one subscription, or more than 64 list IDs or 256 rooms in an
account data, receipts, typing or profiles filter. Lists budget apart when no
room can match both: opposite `is_dm`, `is_encrypted` or `is_invite`, or a
required tag or room type the other excludes or does not admit. Lists are
compared in pairs, so three lists no one room matches together can still share
one budget (an unfiltered list beside a favourites list and a not-favourites
list). Repeated ranges and filter entries count, and are kept, once. On a
resumed connection the check counts the lists and settings the request
inherits; a connection read from storage is checked before it is cached, and
both checks run before presence is updated and before the connection is reset,
advanced or stored, so a client can correct the request and retry at the same
position. A connection stored before this change can hold selections that are
now refused; a request without `pos` starts it afresh. Each pass numbers the
lists' distinct selectors once, so a room merges each selector once however
many of its lists repeat it.
The fork first kept only the first entries instead, which could drop a
`not_tags` or `not_room_types` exclusion and let a room the user can see into
a list that excluded it (#174). A `spaces` entry also looks up the one
`m.space.child` event naming the room instead of reading the space's whole
state. Upstream has the same bug; the check follows the upstream proposal
matrix-construct/tuwunel#732, covering upstream's MSC4480 `sticky_events` scope
(`443a6dd30`) alongside the other extensions, and with the grouping and
distinct counting above, so that
it refuses little the fork accepted before: only the three-list case above
and selections it used to cut. Files:
`src/api/client/sync/v5/{limits.rs,selection.rs,rooms.rs,range.rs,filter.rs}`,
`src/api/client/sync/v5.rs`, `src/service/sync/mod.rs`,
`docs/troubleshooting.md`; tests in
`src/api/client/sync/v5/limits/tests.rs`,
`src/api/client/sync/v5/rooms/tests.rs`, `src/service/sync/tests.rs`,
`src/main/tests/sync_v5_request_limits.rs` and
`src/main/tests/sync_v5_list_filters.rs`.

### Sliding sync deduplicates repeated ranges

Upstream `928a4abd2` (PR #715) and `2726515ec` now copy each sliding sync
window room once in `selector.rs`. The fork keeps range deduplication in the
connection state so a list stores each distinct range once instead of repeating
duplicate ranges sent in a request. Files: `src/service/sync/mod.rs`; test in
`src/service/sync/tests.rs`.

### A multi-word search reads the shortest list first and looks up rare hits

A room search for several words read each word's hits, newest first, and kept
those of the first word that every other word's list held. Each candidate
copied all the other lists before scanning them, about 3.1 billion ids for a
ten-word sentence of common words with hit lists of roughly 200,000 events,
and the whole search request took about 47 seconds in one room while a
single word took under a second. A first fix checked the shortest list
against a hash set of every other list, but still read every list whole,
about 520,000 postings per page for that sentence in one busy room. Now the
first 16 words' lists are read one id each in turn until the shortest ends,
and later words are opened only on their turn. Each other list is read on
while it holds at most `PROBE_FACTOR` (4096) times the hits left; one that
ends by then is intersected by hash set, and past that each hit's key for that
word is looked up instead. The factor was 8 at first, but on a production-sized
index a lookup costs about 7 ms of CPU, some 4,000 times a scanned posting, so
a rare word searched with a common one took 15 s instead of 0.5 s. A repeated
word is read once. Index keys grow instead of panicking on a word that
lowercasing lengthens. Up to four rooms are searched at once, in their order.
Ids, order, `count` and paging are unchanged. A lookup that fails for a reason
other than a missing key is logged and leaves that room out of the results,
where a failed scan item was skipped. In a release benchmark on synthetic
production-sized lists, with the factor at 8, that sentence's id search fell
from 163 ms to 4 ms with a warm block cache, and from about 9 s to under 0.4 s
on its first run after opening the database. At 4096 that sentence reads
every list whole again, so those gains no longer apply to it; lookups now
help only a room with very few hits against a huge list. Upstream has the
same bug. Files: `src/service/rooms/search/mod.rs`,
`src/api/client/search.rs`; tests in
`src/service/rooms/search/tests.rs`, `src/service/rooms/search/tests/timing.rs`
(synthetic production-sized lists intersect in under five seconds of Linux
thread CPU time, where the old code took 35 to 49 seconds) and
`src/main/tests/search_multi_word.rs`.

### Uploaded device and cross-signing keys are bounded in size

Device keys a local client uploaded, and its master and self-signing keys,
were stored with no size limit and sent to other servers unchanged in
`m.device_list_update` and `m.signing_key_update` EDUs. Device keys, including
those of a dehydrated device, and each cross-signing key are now refused with
413 `M_TOO_LARGE` over 64 KiB, the limit upstream and Synapse apply to
to-device messages. Upstream
bounds to-device messages, device IDs and the EDU bytes of a federation
transaction since v1.9.4 (`ff77c2220`), but still stores and sends keys of any
size. Files:
`src/api/client/{dehydrated_device/put_dehydrated_device.rs,keys/upload_keys.rs,keys/upload_signing_keys.rs}`,
`src/service/sending/{mod.rs,sender/mod.rs}`; test in
`src/main/tests/edu_content_size.rs`.

### Presence status messages are bounded in size

A presence `status_msg` was stored at any size the request body limit allows
(24 MiB by default), from a local client or in a federated presence EDU. Every
user sharing a room with its sender then received it in each sync that carried
the sender's presence, decoded and encoded again per sync, so one large status
used a large share of the server's memory once the room's members synced. A
local status message over 4 KiB is now refused with 413 `M_TOO_LARGE`, and a
longer one in a federated update is dropped while the rest of the update
applies. Status messages are a line of text: the MSC4426 profile status allows
256 bytes, and a MindRoom agent's status, naming its model, role and tools,
takes a few hundred. Upstream has the same bug. File:
`src/service/presence/pipeline.rs`; test in
`src/main/tests/presence_status_size.rs`.

### The ignored user list is bounded in size

A user's `m.ignored_user_list` was stored at any size the request limit allowed
(24 MiB by default). The whole list is read and parsed for every event another
user sends in a room the owner is in, and no later change on the server reaches
`/sync` until that is done. A list of 24 MiB took about 0.2 s to parse, so each
such event held up sync for everyone. The list is now refused with 413
`M_TOO_LARGE` over 1 MiB, which holds about 4,000 user IDs of the maximum
length and over 30,000 of ordinary length, and parses in a few milliseconds.
Upstream has the same bug; it bounds push rules, which are matched per event
the same way, since v1.9.0. File: `src/api/client/account_data/mod.rs`; test in
`src/main/tests/ignored_user_list_size.rs`.

### Federated read receipts need a joined user

An `m.receipt` EDU from another server was stored for any of that server's
users once it had a member in the room, whether or not the named user was
joined, and for any `thread_id` it named. Users outside the room then showed up
as readers, and each new user or `thread_id` string stored another receipt row,
kept until the room is deleted. A federated receipt is now stored only for a
user joined to the room, as typing notifications already require, and with no
`thread_id`, `main`, or a thread root that is an event of the room. Its event id
is not checked, because a receipt can arrive before its event or from a server
other than the event's; a receipt for an event this server does not have yet
replaces the user's receipt for that thread like any other. Upstream has the
same bug. Files:
`src/api/federation/send.rs`, `src/api/client/read_marker/mod.rs`; test in
`src/main/tests/federation_receipt_edu.rs`.

### A receipt recounts only the unread events after its event

An older receipt used to clear every unread count in its context. Both
`/receipt` and `/read_markers` now recount notified events after the event they
acknowledge, restricted to the room and receipt context. Counts only decrease;
when both read-marker fields advance, only their furthest accepted event is
recounted. Own sends keep #139's context selection and still read the present,
without a snapshot.

The recount uses the existing room timeline range and visits at most 4,097
PDUs, including sibling threads and events without notification actions. Past
that it reads its context through the present, as every receipt did before: a
receipt on the newest event of its context always clears it, and only an older
receipt in a very busy room clears too much. No scan runs when every context
of the receipt is already at zero. Reading a thread removes its count row
instead of writing a zero, so rows exist only for threads with unread
notifications; an unthreaded receipt visits each of the room's thread rows,
removes the zero rows older servers left behind, and, as before, removes the
user's thread read stamps in the room.

The recount scans without the insertion lock, then checks
notification/highlight counts and read stamps under that lock before applying
a result. The room stamp is read once per phase, and a read stamp takes its
sequence number only for its own write, after the counts it covers, so a
receipt's recount holds no sequence number: syncs wait on it only while a
send, or a redaction settling the counts of the event it redacts, holds its
own sequence number and waits for the lock. Own sends and joins recount while
holding their event's sequence number. A concurrent insertion or read keeps
that context's counts unchanged. Accepted trade-off: when two receipts of one
user race, the later one can be dropped (a main receipt also invalidates a
concurrent thread receipt), and because it was stored, the count stays until
the next event in that context. Notification actions and both count
increments share the lock, and so does upstream's removal of a redacted event
from both counts (`0177e2de8`). No column or index is added. Per-thread
suppressed pushes are a separate draft change. Inbox read flags go by the sync
counts; see "`/notifications` reads entries by their timeline's sync count".
Tests cover old receipts, both marker fields (also with more than 4,097 events
between them), cross-room traffic, overflow, receipts on the newest event
behind 4,097 later events, read threads leaving no rows or stamps, syncs
racing an unthreaded receipt over 4,097 thread rows, and a concurrent send
behind 100,000 sibling rows.

### A withdrawn knock does not move a former member's departure

Under `shared` history visibility a former member reads events up to their
latest leave. Knocking again after leaving or being kicked dropped that leave,
and withdrawing the knock recorded a new one, so the user then read everything
sent since their removal; rejecting a later invite did the same. A former
member now sees an event they were not joined for only up to the leave or ban
that ended their last join, found by walking back from their current
membership event to that join. Upstream has the same bug. File:
`src/service/rooms/state_accessor/user_can.rs`; test in
`src/main/tests/knock_withdrawal_history.rs`.

### A user's own event reads only its own thread

The server marks each event read for its sender. It did that as an unthreaded
read, inherited from Conduit: any event zeroed the sender's counts for the
whole room, the main timeline and every thread, and moved their private read
marker past all of them. Threads the user never opened then disappeared from
badges, push counts and the user's other devices, while their client still
showed them unread. MSC3771 reads each thread on its own, so a local sender's
own event now counts like a receipt in the context it belongs to. An event in
a thread (a reply, or a reaction, edit or redaction of a thread event) resets
that thread's counts and private read marker; any other event resets the main
timeline's, relations to a thread root included. That is how Synapse assigns
an event with an `m.relates_to` to a thread for push actions. Synapse puts a
redaction in the main timeline, while this fork resolves it through the
redacted event, as its pusher already did.

A state event reads nothing: a topic, a thread tag (MindRoom resolves and tags
threads with state events), or a profile change, which sends a member event to
every joined room. Only the user's own join still reads the whole room. Synapse
resets nothing on a send: its counts follow receipts only, it never counts the
sender's own event, and without an unthreaded receipt it counts from the
user's current membership event. Events from remote senders mark nothing.

The server writes the sender's private read marker without announcing it, so
an explicit private receipt for the same event was refused as not newer and
never reached the user's other devices. An announced private receipt now
publishes a position the marker already holds unless it was announced before;
it resets no counts then, since later events may have arrived, and a repeat
still changes nothing.

Upstream has the same bug: every send resets the whole room. Files:
`src/service/rooms/timeline/append.rs`,
`src/service/rooms/read_receipt/data.rs`; test in
`src/mindroom-tests/tests/sync_thread_counts.rs`. Upstream's
`src/main/tests/private_receipt_room.rs` took its unthreaded private markers
from the user's own sends; it now sets them with receipts first.

### send_join state keeps other local users' memberships

Forcing the `send_join` state replays each `m.room.member` event in it, so a
join the authorization rules accept for another user of this server marked
that user as joined. Up to room version 10 such a join passes from any sender
when its only previous event is a create naming that user as creator, and an
old join of a user who has since left passes too. A join of a local user other
than the joining user is still stored, as other events may name it, but is now
left out of the room state. The forced state also replaced the room's, so a
local user the answer left out lost their leave from it while still recorded as
left, and a later event whose state had their old join joined them again. The
member event the room's state has for a local user other than the joining user
now stays, unless the answer has later ones for them that follow it through
their auth events, such as an unban and then a new invite. A remote knock no
longer forces any state since upstream `a2f26001b`. Upstream has the same bug.
Files: `src/service/membership/join.rs`; tests in
`src/service/membership/join/tests/send_join/mod.rs`.

### Deleting an alias by power level takes room membership

`DELETE /directory/room/{alias}` let anyone holding the room's
`m.room.canonical_alias` power level delete an alias they did not create,
whether or not they were in the room. A user who had left, been kicked or been
banned with a level still on record, or any local user for a room whose
`users_default` meets the level, could delete its aliases. Apart from the alias
creator and server admins, the user must now also be joined to the room, as
Synapse requires. Upstream has the same bug. File:
`src/service/rooms/alias/mod.rs`; test in
`src/main/tests/alias_delete_membership.rs`.

### Auth chain fetch walks are bounded

Fetching the missing auth events of an incoming event walked the remote
server's auth chain one event at a time and kept every fetched event in memory
until the walk ended, with no limit on the number of events and only the
federation response limit (256 MiB by default) on each one. A walk now gives up
and drops what it fetched once it holds
`federation.history.max_fetch_prev_events` events (default 1024) and would
fetch another. The walks for one event's auth events, or for the events of one
`/state_ids` answer, keep what they fetched until all of them end, so they also
share one limit of `federation.history.max_fetch_prev_events` times 64 KiB (64
MiB by default) on the events they fetch; a walk whose fetched event would pass
it gives up the same way. A walk keeps each fetched event as canonical JSON
without its `unsigned` field, which the outlier path removes before its own
size check, and parses it again only to authorize it; an event that is then
still larger than the 65,535 byte PDU limit is a failed fetch. The fetcher
rejects an event response larger than four times that limit before parsing it,
which leaves room for the `unsigned` data a server serves with the event.
Upstream has the same bug. Files:
`src/service/rooms/event_handler/fetch_auth.rs`,
`src/service/fetcher/validate.rs`; test in
`src/main/tests/auth_chain_fetch_budget.rs`.

for the `unsigned` data a server serves with the event, and stops reading an
event response once it passes that size instead of buffering it up to the
federation response limit first. Upstream has the same bug. Files:
`src/service/rooms/event_handler/fetch_auth.rs`,
`src/service/fetcher/validate.rs`, `src/service/fetcher/transport.rs`,
`src/service/federation/execute.rs`; test in
`src/main/tests/federation_event_response_limit.rs`.

### An incoming event's fetches share its limits

The walk over an incoming event's missing prev events queued a fetch for every
prev event of each event it walked, up to 20 each, but counted only the walked
events against `federation.history.max_fetch_prev_events`. Every queued event
is fetched in the end, so one incoming event could start about 20 times that
many fetches. The walk now counts every event it queues, and runs at most 32 of
their fetches at once (see the next entry). The `/get_missing_events` batch
fetched before the walk asks for 10 events but stored every event the remote
server answered with; it now stores at most 10. Each auth chain fetch started
for the incoming event, for its own auth events, for each prev event the walk
fetches and for each event of that batch, also had its own limit of
`federation.history.max_fetch_prev_events` times 64 KiB; they now share one.
Upstream has the same bugs. Files:
`src/service/rooms/event_handler/fetch_prev.rs`,
`src/service/rooms/event_handler/fetch_auth.rs`,
`src/service/rooms/event_handler/handle_incoming_pdu.rs`; tests in
`src/main/tests/auth_chain_fetch_budget.rs`.

### A prev event walk runs a few fetches at once

The walk over an incoming event's missing prev events started every fetch it
queued at once, up to `federation.history.max_fetch_prev_events` (default
1024). The fetcher runs at most 256 fetches for all rooms together, so one walk
over a wide gap whose server answered slowly could take all of them, and
fetches for every other room waited behind it. A walk now runs at most 32 of
its fetches at once, more than the 20 prev events one event may name, and
starts the rest as those finish, so a wide gap still fills completely. Upstream
has the same bug. File: `src/service/rooms/event_handler/fetch_prev.rs`; test
in `src/main/tests/auth_chain_fetch_budget.rs`.

### Federation bodies and incoming PDUs are parsed after their checks

A federation request's body was parsed into a JSON tree before the request was
authenticated, and the signature check copied that tree and then waited for the
origin's signing key, which is fetched from the origin when it is not cached. A
body can be up to `max_request_size` (24 MiB by default) and its tree many
times that, so a slow key answer kept two such trees per request until the
request timed out, and a handled request kept its tree until the handler
finished, although no federation handler reads it. The PDUs of a `/send`
transaction, an invite's event and the create event in its stripped state, and
the events of `send_join`, `send_leave` and `send_knock` were parsed at any
length, since the 64 KiB PDU limit is checked only later, and a transaction
parsed all of its PDUs before any of them waited for its room's turn. The
origin's key is now resolved before the body is parsed, and the signature check
uses that key rather than looking it up again, which would fetch a key document
too large to store a second time. A federation request drops the parsed body
once its typed request is built, and an incoming PDU longer than four times the
PDU limit, the limit the fetcher already applies to fetched events, is refused
before it is parsed. A transaction keeps each PDU's JSON raw and parses it
again when its room's turn comes. Upstream has the same bug. Files:
`src/api/router/{args.rs,auth.rs,auth/dispatch.rs,auth/server.rs}`,
`src/api/federation/{send.rs,invite.rs,send_join.rs,send_leave.rs,send_knock.rs}`,
`src/matrix/{pdu.rs,pdu/format/check.rs}`,
`src/service/rooms/event_handler/parse_incoming_pdu.rs`,
`src/service/membership/stripped_state.rs`,
`src/service/fetcher/{validate.rs,transport.rs}`; tests in
`src/main/tests/federation_inbound_parse.rs` and
`src/main/tests/federation_origin_key_fetch.rs`.

### Client bodies are parsed after authentication

A client request's body was also parsed into a JSON tree before the request
was authenticated, on every route. A body can be up to `max_request_size`
(24 MiB by default) and its tree many times that, about a hundred times for an
array of small objects, so a few requests without an access token could take
gigabytes. Only a federation request needs its body before authentication, to
check the signature over it. A client request is now authenticated first, so a
route that needs an account refuses a request without one before its body is
parsed, and a malformed body sent without the credentials a route needs gets
the authentication error rather than `M_NOT_JSON`. A request without an
authenticated user or MAS, as for login, registration and token requests, is
refused with `M_TOO_LARGE` when its body holds more than 1,024 brackets, commas
and colons outside strings; those bodies hold a few dozen values. Upstream has
the same bug. Files: `src/api/router/args.rs`, `src/api/router.rs`,
`src/api/mas/mod.rs`; test in `src/main/tests/client_body_after_auth.rs`.

### Missing-event and state id answers are read up to a limit

An incoming event whose prev events are missing asks the sending server for
them with `/get_missing_events`, and the state fallback asks it for the state
at an event with `/state_ids`. Both answers were read under the generic
federation response limit (256 MiB by default), and each event of a
missing-events answer was parsed in full before anything checked its size,
although the request asks for only 10 events. A missing-events answer is now
read up to the size of one `/event` answer for each event asked for (about 2.5
MiB for 10), and an event in it larger than four times the PDU size limit is
dropped before it is parsed. A `/state_ids` answer is read up to 64 MiB, the
ids of over a million events. A larger answer is dropped while it is read, and
neither limit exceeds `network.http.max_response_size`. Upstream has the same
bug. Files: `src/service/fetcher/{mod.rs,transport.rs,validate.rs}`,
`src/service/rooms/event_handler/fetch_prev.rs`; test in
`src/main/tests/federation_event_response_limit.rs`.

### GitHub sign-in uses the account id

GitHub's user API has no `sub`, so a `login` alias made the username the
identity, and GitHub frees a username after a rename or account deletion.
Whoever registered it next signed in to the account linked to the previous
owner, and through the fork's self-reactivation also revived it if it had been
self-deactivated. GitHub identities are now keyed on the numeric account `id`,
hashed under their own issuer string, and the `login` only picks a new
account's localpart. An association stored under a `login` moves to the `id` at
the next sign-in only when the avatar URL stored with it names that `id`;
otherwise the sign-in is treated as a new user, and
`docs/authentication/providers.md` describes how to re-link the account.
Upstream has the same bug. Files: `src/service/oauth/{mod.rs,user_info.rs}`,
`src/api/client/session/sso.rs`.

### SSO sign-in can be assigned an account created ahead of it

An operator migrating from another chat system can create each person's
account before their first sign-in, for example through an appservice's
`/register`, and fill it with history. That sign-in then got a new account,
because an existing account SSO did not create is skipped, and
`trusted = true` hands out any account whose name the claims give. A
provider's `assigned_accounts` maps email addresses, matched ignoring ASCII
case, to local user IDs. The config check requires local IDs and distinct
addresses; the map is read live, so a reload applies. An identity with no
account yet whose `email` claim is listed gets that account and no other:
the sign-in is refused, with a warning naming the reason, when the provider
says the address is unverified (any `email_verified` value other than
`true` or the string `"true"` in any case; an absent or null claim is not
checked), or the account does not exist or cannot be read, is the server
user, is deactivated (an empty password with a recorded reason, or a mark
in the column a migrated database left; anything unreadable counts), or is
linked to any identity, including one adopted from another server's
database. A chained sign-in whose identity has no link yet is refused when
its listed address names another account than the one it carries. The
checks are made under the account's password lock, held until the link is
stored, and an account with an empty password gets the sentinel and origin
`sso` in the same write batch as the link, with a warning; other accounts
keep their password and origin. Because an assigned or claimed account is
already verified to exist and held under that lock, `complete_sso_session`
skips the redundant `users.exists` read before registration, so a storage
read error there cannot enter `register_user` and deadlock on its own
password lock. Every first sign-in that takes an
account by name, new or existing (a new registration, or an identity's
fallback username), now holds that lock from after any registration until
its link is stored and checks the account's link index again under it, so
it cannot link an account an assignment linked meanwhile; such a sign-in
is refused, as is one whose name, chosen as free, was created by someone
else since. An account any provider's map lists is skipped when a sign-in
chooses an account by name or fallback. Trusted providers and admin
associations are otherwise unchanged.
Registration (`full_register`), the admin API's user creation and MAS
provisioning now refuse an account that exists by checking under the
account's password lock (`create_new`), so an account created meanwhile,
by an appservice or a first sign-in, is neither overwritten nor taken.
Deactivation writes the empty password and its reason in one transaction
under that lock. Unlisted addresses and later sign-ins work as before.
Upstream has no such option. Files: `src/core/config/{mod.rs,check.rs}`,
`tuwunel-example.toml`, `src/service/users/{mod.rs,register.rs,sso.rs,tests.rs}`,
`src/api/admin/users/create_or_modify.rs`, `src/api/mas/provision_user.rs`,
`src/service/oauth/sessions.rs`,
`src/api/client/session/sso.rs`, `docs/authentication/providers.md`; tests
in `src/main/tests/sso_assigned_account.rs`, `src/service/users/tests.rs`
and `src/core/config/tests.rs`.

### The SSO grant cookie uses the `__Host-` prefix

The cookie that carries an SSO sign-in from the redirect to the callback is
now `__Host-tuwunel_grant_session`, set with `Secure`, `Path=/` and no
`Domain`, so other hosts under the same domain cannot set it. The callback
reads only that name, and the removal cookie has the same name, `Path=/` and
`Secure`. Every provider now shares one cookie, so of two sign-ins started
at once in one browser only the later one completes. A sign-in started
before the upgrade fails at its callback once and succeeds when retried.
Upstream names the cookie `tuwunel_grant_session` and scopes it to the
callback path (moved by upstream `6e7510b88` to
`/_tuwunel/sso/callback/{idp_id}`, with the legacy unstable path kept mounted
until 3.0 when `callback_url` names it or when `callback_url` and
`well_known.client` are unset). File: `src/api/client/session/sso.rs`; tests in
that file and `src/mindroom-tests/tests/sso_redirect.rs`.

### SSO registration rules gate new accounts and join their rooms

A provider with registration open let any identity it vouched for create an
account, so a Google provider admitted every Google account, and
`auto_join_rooms` joins a new account as itself, which an invite-only room
refuses. A provider's `registration_rules` lists rules, each a set of claims
and rooms. A rule is met when every claim it names has its value: `email`
and `hd` (Google's hosted domain, a new `UserInfo` field) ignoring ASCII
case, the other string claims exactly; `email` counts only when the
provider verified it, as `verified_email` decides. The config check refuses
a rule with no claims, a claim outside `sub`, `preferred_username`,
`username`, `nickname`, `login`, `name`, `given_name`, `family_name`,
`email` and `hd`, and an alias of another server. With rules, a sign-in
that would create an account and meets none is refused with `M_FORBIDDEN`
before the account is made, so no account or hold is left and the address
can sign in once a rule lists it; this includes provisioning an LDAP user
through a provider with `registration = false`. A new account joins the
rooms of every rule it meets, in order and each once, after its link is
stored and the sign-in has passed its checks (`membership::join_with_invite`).
For each room, resolved from its alias, a room this server is not in, the
admin room and a room an upgrade replaced (`m.room.tombstone`, so an old
admin room too) are skipped, and so is an alias the room does not name in
its own `m.room.canonical_alias`: any local user may point an alias nothing
holds at any room, including an alias a rule names before its room exists,
while only the room's members with power set that state. Whoever first
creates a room with such a not-yet-held alias owns a room that rightly names
it, and new accounts are invited there. Room IDs are the robust choice. When
the join rule would refuse the account uninvited, the room's highest-powered
active local member that may invite invites it first: never the server's own
user or an appservice (bridge) account, which have no password, nor a
locked, suspended or external account, so a room whose only possible inviter
is such an account is skipped. Unlike `auto_join_rooms`, which joins as the
user, this acts with the room's power, hence the alias, admin room and
tombstone checks. A failed join is logged and never fails the sign-in. The
rules are read from the live config, so a reload applies to the next
sign-in. Without rules, and for existing accounts, assigned accounts and
chained sign-ins, sign-in works as before. Upstream has no such option.
Files: `src/core/config/{mod.rs,check.rs}`, `tuwunel-example.toml`,
`src/service/oauth/user_info.rs`,
`src/service/membership/{mod.rs,join_with_invite.rs}`,
`src/api/client/session/sso.rs`, `src/api/client/session/sso/native_apple.rs`,
`docs/authentication/providers.md`; tests in
`src/main/tests/sso_registration_rules.rs`,
`src/service/oauth/user_info.rs` and `src/core/config/tests.rs`.

### SSO registration rules can create external accounts

A `public` join rule lets any local account join, and a `restricted` one any
account in a room it names, so join rules alone do not keep accounts made for
people outside an organization out of its rooms: every public room, present
and future, would be open to them. A registration rule with `external = true`
marks the accounts it creates in a new map, `userid_external`, which no
client API writes (`users::{is_external, set_external}`). `full_register`
(`Register::external`) writes the mark right after `create_new`, inside the
SSO identity closure, so no session or login token of an external account
exists unmarked; it then skips the first-user admin grant and
`auto_join_rooms`. An account meeting several rules is external when any is.
The mark is never set or cleared later, by a sign-in or a rule change. Only a
missing row reads as unmarked (`is_missing`): a failed read counts as external,
so a storage fault never lifts the limits. A client that joins a room on its
own, such as an advertised shared-emoji room, fails for an external account
unless a rule invited it there.

The refusals sit where every local event is built,
`timeline::create_hash_and_sign_event`, and read the mark only for an
`m.room.create`, an invite, or a sender's own join, so other events pay
nothing: an external sender's create event is refused with `M_FORBIDDEN`
"This account may not create rooms." (`createRoom` and room upgrades), its
invite with "This account may not invite others." (`/invite`, the state
endpoint, remote invites), and its own join to a room it is neither joined
to nor invited to with "This account may join a room only when invited."
(the state endpoint as well as `/join`). A join it already has stands, so
profile changes still reach its rooms, and leaving, knocking and accepting
invites, auto-accept included, are unaffected. `membership::join` makes the
same join check for every path, remote joins included, whose event comes
from the resident server's template; appservices acting as the account are
refused too. `createRoom` checks first, since it reports its create event's
errors as `M_INVALID_PARAM`. `join_needs_invite` admits an external account
by no join rule, so `join_with_invite` (the rule's rooms) and the admin join
API invite it first; `force-join-room`, which joins without one, is refused.
An external requester's user directory search keeps only users it shares a
joined room with, whatever `directory.users.show_all_local` says. Such a
rule should not list rooms whose members gain privileges by membership: the
rule invites the account there. `external_forbidden_rooms` (below) keeps
external accounts out of such rooms. An external account also creates no room
alias: `alias::creation_check`, which `PUT /directory/room/{alias}` and
`createRoom`'s `room_alias_name` call, refuses it with "This account may not
create room aliases." (`users::external_alias_check`); server-internal alias
creation is unaffected. A third-party invite (`m.room.third_party_invite`) is an
invite and gets the invite refusal. The upgrade route refuses an external
requester with the create refusal at its entry, as it names the server user the
creator of an upgraded admin room, whose create event the central check passes.
The `user_can_invite` probe goes through the same builder, so an external
account is never chosen to authorise someone else's restricted join. An
external account is never a server admin: `user_is_admin` is false for it even
in the admin room (so the admin API and admin commands refuse it, and
`user_is_last_admin` ignores it), and the admin grant (`make_user_admin`)
refuses it with "This account may not be a server admin.", so admin routes that
act as the server user cannot carry it out of its rooms. After a room upgrade,
external members cannot join the replacement room by following the tombstone:
the upgrade moves only the creator and the sender (`move_member`), so someone in
the new room invites them.
Upstream has no such accounts.
Files: `src/core/config/mod.rs`, `tuwunel-example.toml`,
`src/database/maps.rs`, `src/service/users/{mod.rs,sso.rs,register.rs}`,
`src/service/rooms/timeline/create.rs`, `src/service/rooms/alias/mod.rs`,
`src/api/client/room/upgrade.rs`, `src/service/admin/{mod.rs,grant.rs}`,
`src/service/membership/{join.rs,invite.rs,join_with_invite.rs}`,
`src/api/client/{session/sso.rs,room/create.rs,user_directory.rs}`,
`src/api/client/session/sso/native_apple.rs`,
`docs/authentication/providers.md`; tests in
`src/main/tests/sso_registration_rules.rs`.

### SSO registration invites admit single addresses at runtime

Registration rules live in the configuration, so letting one more person
register meant editing it and reloading. A registration invite is one more
rule for a single email address, `{ rooms, external }`, kept in a new map,
`registrationemail_invite`, keyed by the address in its canonical form
(`threepid::canonicalize_email`), that applies to every provider and is used
once (`users::{registration_invite, set_registration_invite,
remove_registration_invite, registration_invites}`). `registration_terms`
reads the invite of the address the provider verified, as for a rule's
`email` claim, only for a sign-in that would create an account: on a provider
with rules it admits a sign-in that meets none, and on any provider its rooms
and external mark add to those of the met rules. It never closes a provider
without rules, and the refusal of `registration = false` comes before it and
is unchanged. Only a missing row
reads as no invite: a failed read fails the sign-in, so a storage fault never
registers an invited external person as a normal account. The invite is
removed in `complete_sso_session` once the sign-in stands, beside the room
joins, so a sign-in that failed before that can be tried again.

Server admins manage invites through three routes of the fork's own, under
`/_matrix/client/unstable/io.mindroom/admin/registration_invites`: `GET` lists
them, `PUT .../{email}` writes one (optional `rooms` and `external`) and
`DELETE .../{email}` removes one. `RumaAdmin` refuses an account that is no
server admin with `M_FORBIDDEN` before the request is parsed. `PUT` and
`DELETE` refuse a malformed address (`M_INVALID_PARAM`); `PUT` also refuses
something that is no room (`M_BAD_JSON`), an alias of another server
(`M_INVALID_PARAM`, as the config check does for a rule) and an address an
account already has (`M_THREEPID_IN_USE`, naming the account), and `DELETE`
answers `M_NOT_FOUND` for an address without an invite. The routes are
registered with or without MAS delegation, and `/versions` lists
`io.mindroom.registration_invites` while an identity provider is configured.
No email is sent. The request types are the first `tuwunel_api_admin` defines
itself: ruma's request macros write a type's server half under the `server`
feature of the defining crate, so the crate has `client` and `server`
features (`server` by default and for the workspace), and the workspace's
`check-cfg` list names `ruma_unstable_exhaustive_types`.
Upstream has no such invites.
Files: `src/service/users/{mod.rs,registration_invites.rs}`,
`src/database/maps.rs`, `src/api/client/session/sso.rs`,
`src/api/admin/{mod.rs,routes.rs,registration_invites.rs,Cargo.toml}`,
`src/api/client/versions.rs`, `Cargo.toml`, `src/core/config/mod.rs`,
`tuwunel-example.toml`, `docs/authentication/providers.md`; tests in
`src/main/tests/sso_registration_rules/invites.rs`.

### The user admin API marks external accounts, and `POST /deactivate` refuses an active admin

The mark of an external account (`users::is_external`) reached no admin route,
so an admin client could not tell those accounts apart. An external account now
has `"user_type": "external"` in the items of `GET /_synapse/admin/v2/users` and
`GET /_synapse/admin/v3/users`, in `GET /_synapse/admin/v2/users/{user_id}` and
in the response of `PUT /_synapse/admin/v2/users/{user_id}`, whatever
`user_type` that request sent; any other account has no `user_type`, as
before. The field is Synapse's own for a kind of account and was unset here.
Both lists now honor `not_user_type`, which they ignored while no account had a
type: an account of a named type is left out of the list and of its `total`,
and an empty string names the accounts without a type, as in Synapse.
`POST /_synapse/admin/v1/deactivate/{user_id}` refuses another active server
admin and the admin who asks with `M_FORBIDDEN`, after its other checks, as the
lock and suspend routes of MSC4323 do: a deactivation leaves every room, the
admins room too, and clears the profile, and reactivating restores neither.
The deactivation itself looks for the admin (`DeactivationGuard::Admins`),
under the admins room's lock that a promotion holds, so an account promoted
while the request ran is refused with its devices and password untouched. An
admin who was deactivated without leaving the admins room is no active admin,
and is deactivated again or erased as before.
Only this `POST` route asks for the guard.
`PUT /_synapse/admin/v2/users/{user_id}` with `deactivated` set still
deactivates an admin, as do the admin room's command and an account's own
`/account/deactivate`; the last active admin stays refused everywhere.
Upstream marks no external accounts, ignores `not_user_type` and deactivates
an admin through the `POST` route.
Files: `src/api/admin/users/{mod.rs,list_users.rs,deactivate_account.rs,create_or_modify.rs}`,
`src/service/users/{mod.rs,sso.rs}`, `src/service/deactivate/mod.rs`; tests in
`src/main/tests/admin_users_external.rs`.

### SSO assigned accounts can require claims

A provider's `assigned_accounts` gives a listed address's first sign-in its
account on the `email` claim alone, so it was safe only with an app restricted
to the organization. An app that admits outside people too, through
`registration_rules`, accepts any account, and Google lets a personal account
hold a Workspace organization's address: it sends that address, verified, but
no `hd`. Such a sign-in got the listed account. A provider's
`assigned_accounts_claims` names claims every sign-in of a listed address must
all hold, with a registration rule's claim names and matching
(`UserInfo::meets`); the config check refuses an unknown name as it does in a
rule. `assigned_account` reads the account and the claims from one live config
lookup, and `complete_sso_session` refuses a listed address that lacks one with
`M_FORBIDDEN` and a warning naming the account before it selects any account:
the listed one, one the identity is already linked to (also by adoption, or
from before the claims were set), or one a chained sign-in carries. So it
neither takes an account by name nor registers. Set it before the app admits
accounts outside the organization: the check reads the address each sign-in
sends, so an identity whose address changed after it took a listed account is
not checked, and a refusal ends no existing session. If an outside account may
already hold a listed one, delete that identity's link
(`!admin query oauth delete`) and log out the account's devices. A reload
applies to the next sign-in.
Empty, the default, sign-in works as before; unlisted addresses are unaffected.
Upstream has no such option. Files: `src/core/config/{mod.rs,check.rs}`,
`tuwunel-example.toml`, `src/service/oauth/user_info.rs`,
`src/api/client/session/sso.rs`, `src/api/client/session/sso/native_apple.rs`,
`docs/authentication/providers.md`; tests in
`src/main/tests/sso_assigned_account.rs` and `src/core/config/tests.rs`.

### A first SSO sign-in can claim an account an appservice made ahead

`assigned_accounts` needs every person's address in the configuration, so each
new person is a configuration change. Without an entry, the first sign-in of a
person whose account an appservice made ahead got a new account under a
generated name, linked to the identity from then on.

Nothing stored says who made an account: a registration with
`m.login.application_service`, an ordinary registration without a password,
the admin API's user creation and a guest all leave the origin `password` and
an empty password. A new map, `userid_appservice`, records the registering
appservice's ID. `full_register` (`Register::appservice`) writes it right
after `create_new`, and the `/register` route passes the ID only for a
registration of the `m.login.application_service` type, never a guest's.
Nothing else writes it and it is never inferred or backfilled
(`users::registering_appservice` reads it; an unreadable record counts as
none), so an account made before the map existed has none.

A provider's `claim_accounts_of_appservice` names an appservice by its ID. A
first sign-in of an identity linked to no account, with no listed address, no
carried account and no admin association, then gets the existing account named
by the first claim of `userid_claims` the identity holds (`userid_choices`,
which `decide_user_id` now reads too), lowercased in ASCII only, so a
non-ASCII lookalike that lowercases to a name names no account.
`claimed_account` gives it only when the record names the configured
appservice, which is registered and has the account in its `users` namespace
now; the name is in no registered appservice's exclusive namespace (read in the
same appservices read as the record's check), so an import appservice's ghost
accounts there are never claimed: name selection skips names reserved at
lookup, for a claim as for a new account's name; the account is no
appservice's own user, not a server admin, not locked, not of a forbidden name
(matched on the local part) and not listed in an `assigned_accounts`; the
sign-in holds every claim of `assigned_accounts_claims`; the provider does not
say the address is unverified, and says it is verified when the name is read
from the `email` claim; and, by the assignment's own checks (`sso_assignment`,
split from `assign_sso_account`), the account exists, has an empty password,
no recorded deactivation and no linked identity. The record and the
appservices are read first, and the appservices released, before the account
is held: registering an appservice holds them while it takes its own user's
lock, so a claim that held the account and then waited for them could wait on
such a registration for ever. The account's password lock is then taken and
held until the link is stored, and the account gets the sentinel and origin
`sso` in the same write batch, as an assigned account does. The admin and lock
reads are made under that lock, which a promotion or an account lock does not
take. A first claim that is not a valid name claims nothing; no later claim is
tried, as one is for a new account's name. The server reads a passwordless
account outside an exclusive namespace as a guest (`users::is_guest`); the
claim relies on the record instead, and guest handling is unchanged elsewhere.

v2.0.0-mindroom.4 and .5 also claimed an account whose name is in an exclusive
namespace. Accounts meant to be claimed must now be in a namespace that is not
exclusive. A refused claim gets the account the sign-in would get without the
option: ordinary naming tries the later names of `userid_claims` and by default
ends at a generated fallback name, and with `unique_id_fallbacks = false` it
may refuse the sign-in, linking nothing. Only when ordinary naming succeeds is
its link stored, for good until an admin deletes it. Known limits: the
exclusive namespaces are checked at lookup, as ordinary naming checks them, and
the appservices are not held until the link is stored. An appservice registered
before a claim's lookup is seen; one registered after it has the same result as
one registered just after the link, and registering an appservice never
revisits accounts already claimed in its namespace. So operators register an
exclusive namespace, or a wider one, before its new ghost accounts exist or
before setting the option. To widen one live, they pause first sign-ins on
every provider that can name an account in the newly reserved range, let
callbacks under way finish, check the existing links in the newly
reserved range, register, then resume. Unsetting the option is no such pause:
it does not wait for claims already past their lookup, and first sign-ins
meanwhile get ordinary naming instead.

Any other first sign-in is mapped as without the option; when an existing
account of its name is not claimed, a warning names it and the reason, and a
claim is logged at info. The option, `assigned_accounts_claims` and the
claims that name the account are all read from the live configuration: the
provider a sign-in carries is cached as it was first used, so a reload that
set `userid_claims` would otherwise still claim by a name the identity's user
chose. The fallback after a refused claim reads the cached provider, as it
does without the option: a provider that was `trusted` when first used stays
so until a restart. `claimed_account` reads all of that before it awaits
anything, so a reload during a sign-in changes none of it, and waits for the
appservices to be loaded, so one loaded later at startup is still asked
whether the account is its own user. The config check refuses the option
without `userid_claims`, so only claims the operator lists there name an
account (list only claims the organization assigns), and with `trusted`,
whose matching by name would give out an account a claim was refused. The
record proves the appservice, not the person: only the name is compared, not
the address's domain, and the appservice can still act as an account in its
namespace after it is claimed. Unset, the default, sign-in works as before.
Upstream has no such option. Files: `src/database/maps.rs`,
`src/core/config/{mod.rs,check.rs}`, `tuwunel-example.toml`,
`src/service/users/{mod.rs,register.rs,sso.rs}`,
`src/api/client/register/register.rs`, `src/api/client/session/sso.rs`,
`docs/authentication/providers.md`; tests in
`src/main/tests/sso_assigned_account.rs` and `src/core/config/tests.rs`.

### Configured server admins do not need to join the admin room

`server_admins` in `[global]` is an optional, startup-only list of local
Matrix user IDs. For example, `server_admins = ["@helper:example.com"]` gives
that account full server-admin permissions once provisioned, without joining
it to `#admins`. Listing an existing member does not remove its membership or
stop it syncing the room; remove membership separately. Nonmembers do not
receive the room in normal sync, but admins can still read it through the
admin API. An absent ID or external account gains no privileges. Invalid or
remote IDs and the server user are refused in config.

Provision and control listed accounts yourself and keep them out of appservice
namespaces; the server warns about namespace overlaps when loading or
registering an appservice. Sign-up and username availability checks treat
listed IDs as reserved, but operators can still provision them directly.
Appservices cannot act as an unprovisioned listed user, including by targeting
its account data or profile. Startup fails if an appservice registration names
an unprovisioned listed sender. First-time SSO username mapping skips listed
IDs even when an account already exists. Config grants do not authorize chat
admin commands. First-user promotion skips listed IDs; it and last-admin
protection still count only admin-room members.

LDAP does not change a listed user's admin-room membership. Shared-secret
registration and v2 user updates with `admin: true` do not join a listed
account to the admin room. A v2 user update refuses external accounts with
`admin: true` and listed IDs with `admin: false` before any write. Remove the
ID and restart to revoke the config grant, and remove any existing admin-room
membership separately.

Files: `src/core/config/`, `src/service/{admin,appservice,users}/`,
`src/api/{admin,client,mas,oidc,router}/`, `docs/authentication/ldap.md`,
`tuwunel-example.toml`; tests in
`src/core/config/tests.rs` and
`src/main/tests/{config_server_admin_availability,config_server_admins,sso_fallback_account}.rs`.

### External accounts are kept out of listed rooms

An external account joins only rooms it is invited to, but nothing kept it out
of a room whose members gain privileges by membership: an admin's registration
invite or rule could list such a room, and any member with invite power could
invite the account there by hand. The global `external_forbidden_rooms` lists
rooms an external account may never be in, as room IDs or aliases of this
server. `users::external_forbidden_room` compares a room ID with the list read
from the live config: a room is listed when it is named as listed or when its
ID matches an entry (each alias resolved through `resolve_local_alias` when it
checks). `createRoom` registers a new room's alias only after the room's
events, so with a listed `room_alias_name` it refuses a request that names an
external account, in `invite` or as a member event in `initial_state`, before
the room exists (`external_forbidden_alias_check`). The timeline's
`external_sender_check`, which every local event passes, refuses an
invite whose target is an external account into a listed room, whoever sends
it, admins included, so `/invite`, the state endpoint, the admin join API's
invite and the invite a rule's or registration invite's room sends at account
creation all meet it; the latter is logged and skipped, and the account is
still created. `external_join_check`, which the timeline and `membership::join`
run, refuses an external account's join of a listed room it is not yet joined
to, invited or not, so a remote join meets it too; a join it is already joined
with stands, as a profile change resends it. Members already in a listed room
are not removed: an admin removes them by hand. `PUT
.../registration_invites/{email}` with `external` refuses a listed room, by ID
or by an alias that resolves to one, with `M_FORBIDDEN` naming the room as
given, and `GET .../registration_invites` adds `external_forbidden_rooms`, the
listed rooms as IDs, for a client's room picker. The config check refuses
another server's alias, and an external registration rule naming a listed room
by the same identifier. Empty, the default, nothing changes. The federation
invite route runs the same invitee check, so a remote server's invite is
refused there; an invite a remote server sends as an event of a room this
server is already in is stored, and only the join is refused. Upstream has no
external accounts. Files: `src/core/config/{mod.rs,check.rs}`,
`tuwunel-example.toml`, `src/service/users/sso.rs`,
`src/service/rooms/timeline/create.rs`,
`src/api/admin/registration_invites.rs`, `src/api/federation/invite.rs`,
`src/api/client/room/create.rs`,
`docs/authentication/providers.md`; tests in
`src/main/tests/sso_registration_rules/{forbidden_rooms.rs,invites.rs}`
and `src/core/config/tests.rs`.

### External accounts are shown only their rooms

An external account could join no room uninvited, yet was shown every one: the
room directory listed it each published room with its name, topic, avatar and
member count, a room summary described any room with a `public` or knock join
rule, and a space hierarchy walked every such child. A client's room list
therefore told people outside an organization the names of its rooms.
`users::external_sees_room` is true unless the account is external and neither
joined nor invited, and is applied where a room is described to someone not in
it. The room directory (`get_public_rooms_filtered_helper`, which now takes the
asking local user) drops the other rooms before a chunk is built, so a search,
a page and `total_room_count_estimate` treat the directory as holding only the
account's rooms; a query naming another server returns an empty listing and
asks nobody. A room summary of another room is refused with `M_FORBIDDEN` "This
account may see a room only when it is in it or invited to it.", before the
room's servers are asked for it (an alias of another server is resolved there
first). In a space hierarchy no join rule makes a room accessible to it
(`spaces::is_accessible_child`), so a space it is not in is inaccessible and no
other child is described; a room this server is not in, and the account is not
invited to, is inaccessible to it without another server being asked
(`spaces::get_summary_and_children`). The federation directory routes pass no
user and are unchanged.

A room summary also took requests without an access token, and answered them
for any room with a public join rule, by ID or alias, asking other servers
about remote ones. It now follows the directory: without
`directory.rooms.allow_without_auth` (off by default) a request with neither a
user's nor an appservice's token is refused with `M_MISSING_TOKEN`.

Not covered:

- An alias still resolves to its room ID, and `GET /directory/list/room/{roomId}`
  still says whether a room of a known ID is published.
- A space the account is in or invited to names its children's room IDs: the
  hierarchy returns the space's `m.space.child` events as `children_state`
  unfiltered, since a member reads the same events as the space's state.
- A room with `world_readable` history can be read by its ID by any account,
  and a room that allows knocks shows a knocker its stripped state.
- With `directory.rooms.allow_without_auth` the directory and summaries are
  open without a token, to these accounts as to anyone.
- A bridge's answer to a third-party location lookup is passed on as it is.

Upstream has no such accounts and serves a summary without a token.
Files: `src/service/users/sso.rs`, `src/api/client/directory.rs`,
`src/api/client/room/summary.rs`, `src/service/rooms/spaces/mod.rs`,
`src/api/federation/publicrooms.rs`, `src/core/config/{mod.rs,directory.rs}`,
`tuwunel-example.toml`, `docs/authentication/providers.md`; tests in
`src/main/tests/sso_registration_rules/room_visibility.rs`,
`src/service/rooms/spaces/tests.rs`, `src/main/tests/smoke.rs` and
`src/main/tests/federation_lookup_response_limits.rs`.

### Sliding sync receipts and typing only for joined rooms

Simplified sliding sync sent other users' read receipts and typing for every
room in the window: rooms the user has left or been removed from, which a room
subscription keeps there, and rooms they are invited to or have knocked on.
Typing now follows the required-state rule, so only joined rooms, and
world-readable rooms peeked without a membership, carry it, as v3 `/sync` sends
ephemeral events only for joined rooms. Receipts follow the same rule, except
that a room the user left or was removed from keeps the receipts sent before
their departure, the leave or ban that ended their last join, as its timeline
does; upstream delivers those on the departure sync. A user who never joined,
such as one who rejected an invite, gets none. The user's own private read
receipt and room account data are unchanged. Upstream has the same bug. Files:
`src/api/client/sync/v5/{range.rs,rooms.rs,extensions/typing.rs}`; test in
`src/main/tests/sync_v5_departed_ephemeral.rs`.

### A withdrawn knock does not move a former member's state snapshot

Upstream serves a former member the room state as of their departure
(`750dbc0b9`, `ed0ec6157`, and for legacy `/sync` left rooms `afe2bb529`), but
takes the departure from the leave or ban in current state. Knocking again
after leaving or being kicked and withdrawing the knock, or rejecting a later
invite, records a new leave, so `/state`, `/state/{eventType}/{stateKey}`,
`/members`, `/initialSync`, `/context` and legacy `/sync` left rooms then
served the state as of the withdrawal, with the renames, topics, power levels
and members added since the user's removal. The departure is now the leave or
ban that ended the user's last join, found by the walk that bounds their
history (see "A withdrawn knock does not move a former member's departure"). A
walk past 16 membership events fails the read; a legacy `/sync` left room then
still comes, with only the user's own membership as its state, where upstream
would fall back to the state at the withdrawal. A leave or ban with no recorded
state snapshot, as a state reset or a backfill can leave, ends the walk as the
departure, the bound upstream gives every former member; any other member event
without one fails the read. Sliding sync stops a left room's receipts at the
same departure. While a knock is pending the user may not read the state, as
upstream decides. Upstream has the same bug. Files:
`src/service/rooms/state_accessor/user_can.rs`,
`src/api/client/sync/v3/left.rs`; tests in
`src/main/tests/{knock_withdrawal_history.rs,sync_v5_departed_ephemeral.rs}`.

### Bundled edit aggregations follow the requester's visibility

Upstream `563c670b5`/`7752984d2` now enforces requester visibility on bundled
`m.thread` summaries (serving visible pre-leave replies). The fork keeps
visibility enforcement for bundled `m.replace` edits: `newest_replacement`
checks `user_can_see_event` and `erased_for` so a former member or requester
with erased-sender filtering does not receive an edit hidden by history
visibility. Upstream has the same gap on edit bundles (open PR #707). File:
`src/service/rooms/pdu_metadata/bundling.rs`; test in
`src/main/tests/bundled_relations_after_leave.rs`.

### The last `/relations` page has no `next_batch`

Every non-empty `/relations` page carried a `next_batch`, also the page that
reached the end of the relations, as the first page of a short thread does.
Clients then fetched one more, empty page, and MindRoom Chat showed "Load Older
Messages" above threads whose history was complete and stored the cursor in its
cache. Synapse gives `next_batch` only when more relations follow. A page now
reads one relation past its limit and gives `next_batch` only when that
relation exists, in both directions and with `recurse`. Upstream has the same
bug. File: `src/api/client/relations.rs`; test in
`src/main/tests/relations_next_batch.rs`.

### Recursive `/relations` walks stay bounded and deduplicated

Upstream `cf063b67b` owns the 3-level filter-stops-recursion `/relations` walk
(`RECURSION_DEPTH = 3`) and excludes redacted events (`4121ace12`). On top of
upstream's `walk_level`, the fork keeps three bounds and ordering invariants:
- Normalizes backfilled `from` cursors with `PduCount::into_normal` in
  `relation_keys` so forward/backward seeks stay consistent.
- Deduplicates events reachable under more than one parent (`WalkState.seen`),
  counting and walking each distinct event once.
- Bounds recursive walks at 32,768 distinct events and 262,144 scanned relation
  rows (`Service::requester_relation_rows`, which `requester_relations`
  delegates to): exceeding either limit drops the overflowing level whole
  (`debug_warn!`), and overflowing depth 1 falls back to lazy depth-0 direct
  relations.

Files: `src/api/client/relations.rs`,
`src/service/rooms/pdu_metadata/relations.rs`; tests in
`src/api/client/relations.rs`, `src/service/rooms/pdu_metadata/tests.rs`, and
`src/main/tests/relations_recurse_order.rs`.

### CORS allows the Cloudflare Access token header

MindRoom Chat on iOS reaches a homeserver behind Cloudflare Access by sending its
Access token in a `cf-access-token` header on every Matrix request, because its
web view cannot hold the Access cookie. Access passes CORS preflights through to
the origin, so the homeserver answers every preflight and must allow that header.
The homeserver ignores the header. File: `src/router/layers.rs`.

### OpenID tokens can be bound to a relying party audience

Files:
- `src/api/client/openid.rs`, `src/api/federation/openid.rs`
- `src/api/client/versions.rs`, `src/service/users/mod.rs`
- `src/core/config/mod.rs`, `tuwunel-example.toml`
- `src/mindroom-tests/tests/openid_audience.rs`,
  `src/mindroom-tests/tests/openid_audience_disabled.rs`,
  `src/mindroom-tests/tests/support/openid.rs`

Behavior:
- Everything below applies only with `mindroom_openid_audience = true`.
  The option defaults to `false`, and then the server behaves as upstream.
  `request_token` ignores `io.mindroom.audience` and issues unbound tokens.
  `/openid/userinfo` ignores the query audience, so any valid token verifies.
  A token bound while the option was on verifies like an unbound one.
  `/_matrix/client/versions` does not list `io.mindroom.openid_audience`.
- `POST /_matrix/client/v3/user/{userId}/openid/request_token` accepts an
  optional body field `io.mindroom.audience` naming the relying party (MindRoom
  sends its backend origin). The token is bound to that audience; the response
  is unchanged. Without the field (`{}` or an empty body) the token is unbound,
  as upstream. An audience that is not a string, is empty, is longer than 255
  characters, or contains control characters is rejected with 400
  `M_INVALID_PARAM`. A body that is valid JSON but not canonical JSON (for
  example one containing a float, or an integer outside +/-2^53) is rejected
  with 400 `M_BAD_JSON` instead of issuing an unbound token; with the option
  off such a body still gets an unbound token, as upstream.
- `GET /_matrix/federation/v1/openid/userinfo` reads an optional
  `io.mindroom.audience` query parameter. A bound token verifies only when the
  query names its audience; an unbound token verifies only when the query names
  none. Every other combination is 401 `M_UNAUTHORIZED`, so a relying party
  cannot replay a token bound to it at a deployment with a different audience,
  and a relying party that asks for an audience never accepts an unbound token.
  A repeated query audience is 400 `M_INVALID_PARAM`. Rejected attempts leave
  the token in place; expired tokens are removed as before.
- The `openidtoken_expiresatuserid` value of a bound token is the expiry (8
  bytes big-endian ms), the user ID, a `0xFF` byte, and the audience. `0xFF`
  never occurs in UTF-8, so rows written before this change parse as unbound.
- `/_matrix/client/versions` lists `io.mindroom.openid_audience` in
  `unstable_features`, so relying parties know when to send the query audience.
- The binding also works in the other direction.
  A client that requests a bound token can only use it at relying parties that send the matching query audience.
  A relying party that sends no audience gets 401 `M_UNAUTHORIZED` for a bound token.
  Released MindRoom backends verify Computers OpenID tokens without an audience, so they reject bound tokens until they carry the audience support (mindroom-ai/mindroom#2775).
  Chat clients that have not updated send unbound tokens, which backends that send an audience reject with 401.
- Rollout: deploy this release freely, since the option is off by default and nothing changes.
  Backends and clients that already send audiences keep working while the option is off, because the server ignores both fields.
  Turn the option on only after MindRoom backends with audience support (mindroom-ai/mindroom#2775) and MindRoom Chat clients that send audiences, including the iOS app (mindroom-ai/mindroom-chat#442), are live.
  Turning it off again restores upstream behavior, including for tokens issued bound.

### OpenID userinfo can confirm room membership

Files: `src/api/federation/{openid.rs,routes.rs}`; test in
`src/mindroom-tests/tests/openid_room_membership.rs`.

`GET /_matrix/federation/v1/openid/userinfo` reads an optional
`io.mindroom.room_id` query parameter. With it, the token verifies only while
its user is joined to that room: the response is
`{"sub": ..., "io.mindroom.room_joined": true}`, and a user who is not joined
gets 403 `M_FORBIDDEN`. A server that ignores the parameter never sends
`io.mindroom.room_joined`, so a relying party such as the MatrixRTC
authorization service can refuse calls for non-members instead of trusting
such a server. Without the parameter the response is unchanged. The
route is registered as a plain axum route because ruma's response type has no
room for the extra field.

### Custom ephemeral room events reach the syncs that opt in

MindRoom streams AI replies to MindRoom Chat as small ephemeral frames instead
of frequent full-content edits, so the server relays custom ephemeral room
events: a subset of MSC2477 with a fork-specific opt-in.

Files:
- `src/service/rooms/ephemeral/mod.rs`, `src/service/services.rs`
- `src/api/client/ephemeral.rs`, `src/api/client/routes.rs`
- `src/api/client/sync/v3/{sync_events.rs,joined/mod.rs}`,
  `src/service/sync/watch.rs`
- `src/api/client/versions.rs`, `src/core/config/mod.rs`, `tuwunel-example.toml`
- Tests: `src/service/rooms/ephemeral/tests.rs`,
  `src/main/tests/ephemeral_msc2477.rs`

Behavior:
- Everything below applies only with `mindroom_ephemeral_events_enabled = true`,
  the default. With it off, the endpoint answers 404 `M_UNRECOGNIZED` and
  `/_matrix/client/versions` advertises neither flag below.
- `PUT /_matrix/client/unstable/org.matrix.msc2477/rooms/{roomId}/ephemeral/{eventType}/{txnId}`
  takes a JSON object body from a joined member and answers `{}`. A type
  longer than 255 bytes is 400 `M_INVALID_PARAM`, a type starting with `m.`
  400 `M_UNKNOWN`, a suspended sender 403 `M_USER_SUSPENDED`, a sender who is
  not joined 403 `M_FORBIDDEN`, a body over 65536 bytes 413 `M_TOO_LARGE`, a
  body that is not an object 400 `M_BAD_JSON`, and more than 600 events from
  one sender to one room in a one-second window 429 `M_LIMIT_EXCEEDED` with
  `retry_after_ms` until the window ends. A transaction (type, sender, device,
  txnId) that is still buffered answers `{}` without a second copy.
- Events live only in memory. Each room keeps at most 4,096 events and 4 MiB of
  content, event types, and transaction IDs. Each event is delivered for at
  most 30 seconds, the next send after a minute frees its memory, and a
  restart drops it. Each event takes a global count, as typing does; nothing
  else reaches the database.
- v3 `/sync` delivers an event in `rooms.join.{roomId}.ephemeral.events` as
  `{"type", "content", "sender", "origin_server_ts"}`, without `room_id`, when
  the request has a `since` token with `since < count <= next_batch`, the
  recipient is joined and does not ignore the sender, and the request's
  filter opts in. An initial sync gets none, nor does the first sync after the
  recipient joins.
- Opt-in, the fork extension: `room.ephemeral.types` must be present and match
  the type, exactly or with `*` wildcards, and `room.ephemeral.not_types` must
  not match it. Standard filter semantics read an absent `types` as all types;
  here custom ephemeral events need the explicit opt-in, so clients that do not
  use them (Element, bridges, bots) never receive them. The same rule, for a
  room the recipient is joined to, decides whether a send wakes a long poll,
  so it never wakes theirs either.
- `/_matrix/client/versions` lists `org.matrix.msc2477` and
  `io.mindroom.ephemeral_opt_in` in `unstable_features`; the MindRoom runtime
  sends frames only when the server lists `io.mindroom.ephemeral_opt_in`.
- Omitted from MSC2477: federation, appservice delivery, sliding sync, and
  per-type power levels, which need a new room version. Typing and receipts
  are unchanged.

### `/messages` ends at the room's creation event

A backward `/messages` page that reached the room's creation event still gave
an `end` token, so a client asked again and only an empty page without `end`
ended the history. The spec omits `end` when no further events exist. Synapse
also gives `end` with the page holding the creation event, and omits it on
the empty page after. A backward page now gives no `end` only when its edge
reaches the oldest stored position and that position holds the creation
event. This also handles filters that hide creation and full pages ending
exactly there. Backfill assigns decreasing positions in insertion order, so
a creation event stored before other historical events keeps `end` while
those older positions remain. Rooms whose earlier history is not stored yet
also keep `end`, so the next request can backfill. Regression tests re-key
creation and messages to negative positions in both orders. Upstream has the
same bug. File:
`src/api/client/message.rs`; test in
`src/main/tests/messages_pagination_bounds.rs`.

### The last `/notifications` page has no `next_token`

Every non-empty `/notifications` page carried a `next_token`, also the page
that reached the oldest notification, so clients asked for one more, empty
page, and an inbox kept offering older notifications until then. The spec
gives `next_token` only while more results exist. Synapse also gives it on
every non-empty page. A page now reads one notification past its limit and
gives `next_token` only when that notification exists. Upstream has the same
bug. File: `src/api/client/push/notifications.rs`; test in
`src/main/tests/notifications_next_token.rs`.

### `/notifications` reads entries by their timeline's sync count

`/notifications` marked an entry read once the room's read watermark passed
it. A `"main"` receipt and the user's own main-timeline event move that
watermark too, so they marked every thread reply in the room read while the
sync thread counts still had it unread. A receipt also reads only up to the
event it names, while the watermark moves to its arrival, so after an older
receipt the Inbox showed main-timeline entries read that the room's sync count
still held. An entry now follows its timeline's sync counts, its thread's or
its room's main ones, `N` notifications and `H` highlights, which always hold
the user's newest such events there: the entry is read once `N` of the user's
notifying and `H` of their highlighting events in the same timeline follow it.
One walk down the user's notification rows, to the oldest entry of a timeline
with unread events, serves the whole page and stops once every such timeline
has found them; rows of other rooms are skipped without loading their events.
A notifying event that lands between the count reads and the walk can show the
oldest unread entry of its timeline read until the next poll, for main entries
as for thread entries. Counting notification rows rather than a root's
relations counts finished stream edits like the replies they finish. The walk
skips redacted events, as the recount does; a redacted reply names no thread
and would otherwise count in the main timeline. An event redacted since the
last recount still counts in `N` but not in the walk, so an earlier entry can
show unread until the next receipt, as the count does. An event related to a
thread event by another relation, such as a poll end or a reaction, belongs to
the main timeline once that thread event is redacted, because redaction strips
its `m.relates_to`; MSC3771 clients such as matrix-js-sdk file it there too, and
read it with `"main"` receipts. Its counts were made in the thread. Until a
main or unthreaded receipt recounts the main timeline, the walk finds it among
the main events while the main count does not hold it, so the oldest counted
main entry can show read. File: `src/api/client/push/notifications.rs`; test
in `src/mindroom-tests/tests/notifications_thread_read.rs`.

### The user directory leaves out deactivated accounts

On servers that list all local users (`show_all_local_users_in_user_directory
= true`), `/user_directory/search` otherwise returns deactivated accounts with
the active ones, and with the option off it still returns one that keeps a
room membership. Synapse removes an account from its directory when it is
deactivated; upstream Tuwunel (checked on `main`, `d5151f783`) has no such
filter.

Files: `src/api/client/user_directory.rs`, `src/service/users/sso.rs`
(`deactivation_recorded`, shared with SSO assignment); tests in
`src/mindroom-tests/tests/user_directory_deactivated.rs` (option on) and
`src/mindroom-tests/tests/user_directory_deactivated_rooms.rs` (option off),
with helpers in `src/mindroom-tests/tests/support/directory.rs`.

Behavior:
- An account is left out of every search, whatever
  `show_all_local_users_in_user_directory` and
  `show_appservice_users_in_user_directory` say, when its stored password is
  empty and a deactivation is recorded for it (`deactivation_recorded`, the
  same test SSO assignment uses: a recorded reason, or a mark in the column a
  migrated database left; anything unreadable counts). Every deactivation
  this server performs goes through `deactivate_account`, which records the
  reason, and every reactivation replaces the empty password. Only local
  accounts get a reason; a mark hides an account whatever its server (the
  origin server filled the column with its own accounts).
- An empty password alone does not hide an account: remote users, appservice
  senders, and users created without a password store one too, and keep
  their earlier visibility.
- Accounts deactivated with neither a reason nor a mark stay listed: those
  deactivated before the fork started recording reasons (release
  `v1.5.0-mindroom.16`, 2026-03-04), and deactivations imported from Conduit
  databases. Running `!admin users deactivate --no-leave-rooms <user>` again
  on such an account records a reason and hides it; SSO self-reactivation
  stays refused.
- The appservice filtering and the public-room and shared-room rules are
  unchanged.

### Only a MatrixRTC call leave can be delayed

Upstream's MSC4140 lets a client delay any event. The fork lets a client delay
only its own MatrixRTC call leave, in either of the two MatrixRTC shapes:

1. **Legacy state leave:** an `org.matrix.msc3401.call.member` (or alias
   `m.call.member`) state event with empty content (`{}`), under a state key
   that names the sender's own call slot (the sender's user ID, an underscore
   and the rest, with or without one more underscore in front, as in
   `_@alice:example.org_DEVICE_m.call`).
2. **MSC4354 sticky `rtc.member` leave:** an `m.rtc.member` or
   `org.matrix.msc4143.rtc.member` message-like event carrying a sticky
   duration (`org.matrix.msc4354.sticky_duration_ms`), a non-empty `slot_id`
   and sticky key (`msc4354_sticky_key` and/or `sticky_key`), and either a
   `member` object with `membership: "leave"`, non-empty `id`, optional
   `user_id` matching the sender, optional non-empty `device_id`, and optional
   `leave_reason` (`code` + optional string `reason`), or the legacy
   `{slot_id, sticky_key}` leave shape, with no extra fields.

Every other delayed event is refused with 403 `M_FORBIDDEN` before anything
else is read, whether it comes with the `org.matrix.msc4140.delay` query on
`/send` or `/state` or through the `delayed_event` route, stable or unstable: a
message, any other state event, a delayed sticky `rtc.member` join, a delayed
sticky legacy `call.member` message, a call membership with content, and a
leave under another user's slot.

A MatrixRTC client schedules its own call leave and keeps postponing it, and
that is the only delayed event MindRoom's and Element Call's clients send:
matrix-js-sdk 41.7.0, which MindRoom Chat pins, delays the legacy state leave
from its `MembershipManager` (`clientSendDelayedDisconnectMembership`), and
its `StickyEventMembershipManager` (used by MSC4354 sticky-event clients such
as Element Call) delays a sticky `org.matrix.msc4143.rtc.member` leave now that
upstream implements MSC4354 (`443a6dd30`). matrix-rust-sdk delays only what a
widget such as Element Call asks it to. ruma reads `m.call.member` as another
name of `org.matrix.msc3401.call.member`, so a leave that names it is delayed,
and sent, as an `org.matrix.msc3401.call.member` event.

A delayed event is sent later, without the request an ordinary send passes
through, and upstream's send path rechecks suspension alone. For other events
that leaves four gaps:

- A locked account's delayed message or state event is still sent after the
  lock.
- A scheduling request that authenticated before a deactivation can store its
  event after the deactivation removed the account's scheduled events.
- The state route's checks, such as the recipient's invite preferences for a
  membership and alias resolution for a canonical alias, run when the event
  is scheduled, not when it is sent.
- The record kept of a sent delayed event serves the original content to its
  sender after the event was redacted or purged.

The fork does not close these gaps: the send path, its checks and their order
are upstream's. It limits what can pass through them. The type alone is not
enough for that: a delayed call join, or any other content under that type,
would still be sent after a lock and read back from its record, and room auth
does not tie a state key with an underscore in front to its sender, so a
delayed leave could empty another user's slot. With its own call leave alone,
all that a locked or deactivated account can still have sent takes one of its
own devices out of a call, and the record kept of it holds empty content or a
leave-only sticky payload. Room auth still decides, when the leave is scheduled and when
it is sent, whether the sender may write that state. Upstream's limits,
persistence, management routes and finalised records are unchanged, and the
feature flags and capabilities stay advertised. File:
`src/api/client/delayed_events/mod.rs`; tests in
`src/mindroom-tests/tests/delayed_call_membership.rs` and, for the forms of
the state key, `only_own_call_leave_is_delayed` in
`src/api/client/delayed_events/tests.rs`.

Upstream's `src/main/tests/delayed_events.rs`, `profile_legacy_member.rs`,
`livekit_delegation.rs`, `livekit_webhook.rs`, `state_member_invite.rs`, and
`sticky_events.rs` delayed messages, topics, and room memberships. The route
and LiveKit cases now delay an own-slot call leave and keep what they check;
`sticky_events.rs` keeps upstream's positive delayed-fire and timer tests for
`org.matrix.msc4143.rtc.member` leaves while asserting 403 for sticky
`rtc.member` joins, legacy `call.member` messages, and delayed `m.room.topic`
(testing 400 `M_INVALID_PARAM` on an own-slot delayed state leave with
`sticky_duration_ms`). Three cases in `delayed_events.rs` schedule upstream's
event directly in the service, which is not held to this scope, while
`every_form_schedules` and `state_member_invite.rs` assert 403 for disallowed
delayed events.

### A conflict with a finished delayed event is 404 on the unstable MSC4140 paths

Upstream keeps a delayed event after it is sent, cancelled or failed, and
answers an action that conflicts with that final state with 409 `M_UNKNOWN`: a
restart of any finished event, a cancel of a sent one, and a send of a
cancelled or failed one. The fork's earlier delayed call leave dropped a
finished event, so those actions answered 404 `M_NOT_FOUND`, and MatrixRTC
clients rely on that answer. matrix-js-sdk 41.7.0, which MindRoom Chat pins,
schedules a replacement delayed leave for a call when the restart or cancel of
the old one answers `M_NOT_FOUND`. It retries after a rate limit or a network
error, but it does not handle 409 `M_UNKNOWN`: the restart fails with "Could
not restart delayed event". With upstream's answer, a call whose delayed leave
was already sent, as after a sleeping laptop or a network gap, breaks on its
next restart of that leave instead of rejoining. The SDK calls the unstable
`org.matrix.msc4140` paths. Those paths, in the path form and the earlier body
form, now answer an action that conflicts with the final state with 404
`M_NOT_FOUND`, the answer an unknown `delay_id` gets. The actions that do not
conflict stay successful on every path, as upstream has them: a send of an
event already sent, which sends nothing again, and a cancel of an event
already cancelled or failed. The stable path keeps upstream's 409, and the
finished events stay stored and readable as upstream has them. Only the
service's conflict is translated; a send's own error, such as 403 for a
suspended sender, passes unchanged. File:
`src/api/client/delayed_events/update_delayed_event.rs`; test in
`src/mindroom-tests/tests/delayed_finished_not_found.rs`. Upstream's
`src/main/tests/delayed_events.rs` drives the unstable path, so its
expectations for such a conflict change from 409 to 404 in two lines.

### Incoming-call ring push rules

Files:
- `src/service/pusher/ring_rules.rs`, `src/service/pusher/mod.rs`
- `src/api/client/push/{mod.rs,pushrules.rs,pushrules_global.rs}`, `src/api/federation/invite.rs`
- `src/service/pusher/tests.rs`, `src/api/client/push/tests.rs`, `src/main/tests/pusher_notify.rs`

Behavior:
- Two server-default override push rules sit right before `.m.rule.is_user_mention`
  (or last in `override` without it): `.io.mindroom.rule.rtc.ring` (`type ==
  "org.matrix.msc4075.rtc.notification"`, `content.notification_type == "ring"`) and
  `.io.mindroom.rule.rtc.ring_encrypted` (`type == "m.room.encrypted"`,
  `content.io\.mindroom\.rtc\.notification.notification_type == "ring"`). Both use exact
  `event_property_is` matches, need `<=3` joined members, and have the actions
  `["notify", {"set_tweak": "sound", "value": "ring"}]`.
- `pusher::Service::get_actions` lets a ring rule win only if (a) it is the first rule that
  matches for this recipient, enabled, with `notify` and `"ring"` as its only sound tweak;
  (b) the room's joined count is known and at most 3; (c) no enabled room-kind or override
  mute for this room wins. Otherwise the ring rules are skipped and the unchanged event is
  evaluated exactly as without them. A user turns rings off by disabling the rules or muting
  the room. The rules do not check that a call exists: a member who can send the event can
  ring the others in the room.
- The rules are merged on read, not migrated: a stored ruleset that lacks a ring rule gets it
  for evaluation (`pusher::Service::ruleset`), on both `GET /pushrules` routes and in
  `load_push_rules` (the single-rule routes), with nothing else in the ruleset changed. No
  `GET` stores a missing ring rule; the next rule write stores the merged ruleset. The
  legacy repair of the two full `GET` routes keeps an already stored ring rule with its
  `enabled` and `actions` (`ring_rules::update_with_server_default`).

### 1) `mindroom/edits: compact /sync, purge superseded edits, bundle the survivor`
Files:
- `src/api/client/sync/mod.rs`, `src/api/client/sync/mindroom_edits.rs`
- `src/core/config/mod.rs`, `src/core/config/check.rs`
- `src/matrix/event.rs`, `src/matrix/event/relation.rs`
- `src/database/map/remove.rs`
- `src/service/edit_purge/mod.rs`, `src/service/mod.rs`, `src/service/services.rs`
- `src/service/edit_purge/sweep.rs`, `src/service/edit_purge/tests/sweep.rs`,
  `src/service/edit_purge/tests/references.rs`
- `src/service/media/mod.rs` (uploader-index stream for the sidecar sweep;
  retryable media deletion)
- `src/admin/media/mod.rs`, `src/admin/media/delete_orphaned_long_text_sidecars.rs`,
  `src/admin/tests.rs`
- `src/mindroom-tests/tests/edit_purge_bundle_compose.rs`
- `src/mindroom-tests/tests/orphaned_sidecar_sweep.rs`,
  `src/mindroom-tests/tests/orphaned_sidecar_delete_retry.rs`,
  `src/mindroom-tests/Cargo.toml`
- `tuwunel-example.toml`

Behavior:
- Adds `/sync` timeline compaction for superseded non-state `m.replace` events.
  Redactions are never compacted, even when their content claims a relation.
- Adds a background purge worker that deletes old superseded edit events from
  storage and indexes, retaining the newest eligible edit per (room, target,
  sender). Candidates and originals must be non-state events with matching
  room, sender and type; originals must be readable, match their requested ID,
  and not themselves be edits. Plaintext replacements need an `m.new_content`
  object; encrypted replacement content remains opaque. Invalid or unverifiable
  relationships are preserved rather than used to supersede another event.
- Deletes the MindRoom long-text sidecar media of each purged edit. A sidecar
  is recognized by its `io.mindroom.long_text` marker (version 2,
  `matrix_event_content_json` encoding) with `url` or `file.url`, at the top
  level or in `m.new_content`, whatever the `msgtype`: final `m.file` previews
  and in-progress streaming previews both qualify. Only local media uploaded
  by the local edit sender is deleted, and never media that a retained event
  still references.
- Adds the MindRoom edit-lifecycle configuration surface and purge validation.
- The retained-reference scan that keeps shared sidecars covers every timeline
  PDU, the retained unredacted originals of redacted events
  (`eventid_originalpdu`, kept for `redaction_retention_seconds` when
  `save_unredacted_events` is on and still served to moderators), and outlier
  events. A stored row that does not decode as a PDU is searched as plain JSON.
  The scan yields to the runtime every 1024 rows.
- Adds `!admin media delete-orphaned-long-text-sidecars`, a one-shot sweep for
  MindRoom long-text sidecar media that no retained event references any more,
  such as sidecars of edits deleted before the purge recognized their shape.
  - It only reports unless `--execute` is given. It has its own dry run and
    does not consult `mindroom_edit_purge_dry_run`.
  - Candidates are local unencrypted sidecar uploads (`application/json`
    named `message-content.json`) by local users, optionally limited to
    uploaders whose ID starts with `--uploader-prefix` or matches
    `--uploader-regex` in full (the pattern is anchored).
  - One run of the retained-reference scan above keeps every referenced
    candidate. Each unreferenced candidate is then dated by its storage
    object's modification time and selected if older than `--older-than`.
    At most `--limit` (default 1000) are selected and deleted, through the
    owner-checked media path. The limit bounds deletions and storage-metadata
    lookups for selected candidates, but a candidate found too recent or
    missing from storage does not count toward it, so a run can look up more
    objects than `--limit`.
  - `--older-than` must be at least `mindroom_edit_purge_min_age_secs` plus one
    day. Use a generous cutoff in production (a week or more): a client can
    hold an uploaded sidecar in a durable outbox before sending the event that
    references it.
  - Encrypted-room sidecars are skipped and counted, because encrypted event
    content is opaque to the server. A stored event that cannot be read refuses
    the sweep.
  - The scan reads a database snapshot, so a reference created after it
    starts is not seen. The sweep runs on the admin command processor and
    occupies its queue until it finishes. Scans and deletions yield to the
    runtime.
- Media deletion keeps database rows until storage deletion succeeds.
  `media::delete()` now returns the storage error and leaves the media's file
  and uploader rows in place until every configured provider has removed (or
  never held) the object. Previously a provider failure was only logged and the
  rows were dropped regardless, leaving an unindexed object in storage. This
  affects every deletion caller: the Synapse admin media endpoints
  (`delete_media`, `delete_user_media`, `delete_media_by_date_size` and
  `purge_media_cache`), the
  `!admin media` commands `delete`, `delete-list`, `delete-by-event`,
  `delete-range`, `delete-all-from-user` and `delete-all-from-server`, and the
  purge's and sweep's owner-checked `delete_owned_by`. Media whose deletion
  failed stays indexed and downloadable until a retry succeeds.
- **Turns on upstream's edit bundling by default**
  (`rooms.relations.bundle_edits`, MSC3925; upstream ships it off). The purge
  deletes superseded edits, so without the bundle a history endpoint
  (`/messages`, `/context`, `/event`, ...) would serve an original with its
  stale pre-edit body and no way for the client to find the surviving edit.
  Upstream bundles the newest surviving edit onto the original at
  `unsigned.m.relations.m.replace` via its `relatesto_typed` typed index; the
  purge composes with that index (it tolerates the dangling rows the purge
  leaves behind and always selects the surviving edit), and upstream's startup
  `rebuild_relatesto_typed` migration indexes pre-existing edits.
  `edit_purge::purge_cycle` is `pub` so operators/tests can trigger a cycle; a
  composition test drives a real purge and asserts the survivor is still
  bundled and that a dangling newest index row is skipped.

### 2) `auth/sso: SSO-origin UIAA hardening and self-reactivation`
Files:
- `src/api/router/auth/uiaa.rs`, `src/api/client/session/sso.rs`
- `src/api/client/account/deactivate.rs`, `src/api/client/admin/mas/delete_user.rs`
- `src/api/client/admin/users/deactivate_account.rs`, `src/api/client/admin/users/create_or_modify.rs`
- `src/api/client/membership/mod.rs`, `src/api/oidc/account/account_deactivate.rs`
- `src/admin/user/mod.rs`, `src/database/maps.rs`
- `src/service/deactivate/mod.rs`, `src/service/emergency/mod.rs`
- `src/service/users/mod.rs`, `src/service/users/sso.rs`

Behavior:
- Upstream already ships the strict-CSP-safe SSO UIAA fallback itself (MSC2454:
  server-redirect flow, `m.login.sso/fallback/web` completion, bound-IdP
  routing). Since v1.9.3, `m.login.password` follows upstream's
  credential-matched rule (`0726625e6`): it is offered when the account holds
  a real password, or for an LDAP-origin account while LDAP is enabled, so
  never to a passwordless SSO account. This fork hardens the rest of the
  advertisement: `m.login.sso` only for SSO-origin accounts (never for a
  password account, even on a device an identity provider issued) and only
  when the exact IdP is unambiguous (the device's own IdP or the single
  configured provider), JWT UIAA rejected for SSO-origin users and no longer
  advertised (its fallback/web page is not implemented), and legacy SSO-origin
  account metadata repaired on the fly before the flows are chosen.
- Reactivates a deactivated local SSO account on re-login, but only when the
  account was self-deactivated (a persisted deactivation reason distinguishes
  self-service from administrative deactivation).
- Upstream's Synapse admin deactivation endpoints (the v1 deactivate route and
  the v2 create-or-modify `deactivated` flag, new in v1.8.1) record
  `DeactivationReason::Admin`, so accounts they deactivate stay deactivated on
  SSO re-login. Their full-deactivation path remains upstream's (split into
  helpers with bounded concurrent room departures in v1.9.3, `c53d80d80`, and
  on upstream main refusing the last admin under the admin-room lock,
  `3fe9ef681` and `5420f2e56`, with the reason recorded under that lock):
  users leave joined rooms, and the v1 route forwards its `erase` flag to the
  full cleanup service. The fork adds the reason argument, not a shallow replacement
  for upstream cleanup. Route tests verify room departure for both endpoints.

Design note — why deactivation takes a reason (vs Synapse/upstream):
- Synapse models deactivation as a bare `users.deactivated` flag (plus the
  separate, admin-imposed MSC3823 `suspended` flag); it stores no reason,
  hard-blocks SSO login for deactivated accounts (the
  `sso_account_deactivated_template` 403 page in `auth.py`), and its
  admin-only `activate_account` expects a password hash to be set afterwards,
  which passwordless SSO accounts don't have.
- Upstream Tuwunel likewise stores no reason; since v1.8.1 an admin can
  reactivate a passwordless user via the password sentinel, but neither
  server has a self-service path.
- The fork persists the initiator (`self`/`admin`) so *self*-deactivated SSO
  accounts can safely self-reactivate when the same IdP identity returns,
  while admin deactivations stay final. The reason is a required parameter of
  `deactivate_account` and `full_deactivate`, so each new upstream call site
  must classify itself
  at compile time — this is the recurring (and intentional) rebase seam; the
  v1.8.1 rebase adapted two new Synapse-admin endpoints exactly this way,
  and the v1.9.1 rebase preserves upstream's new full-deactivation routing.
- Deliberately not upstreamed: reversible deactivation changes established
  account-lifecycle semantics and needs upstream design agreement, so we
  carry it as a fork feature. The full comparison lives in
  `src/service/users/sso.rs` above `DeactivationReason`.

Note: the SSO grant-cookie path hardening this fork originally carried (matching
set and removal cookie paths) was merged upstream. The fork now differs on that
cookie again: see "The SSO grant cookie uses the `__Host-` prefix" above.

### 3) `auth/apple: native iOS Apple login exchange`
Files:
- `src/api/client/session/mod.rs`, `src/api/client/session/sso.rs`
- `src/api/client/session/sso/native_apple.rs`
- `src/api/router.rs`, `src/core/config/mod.rs`, `tuwunel-example.toml`

Behavior:
- Adds `POST /_matrix/client/unstable/org.mindroom.login/apple`.
- Verifies native Sign in with Apple identity tokens against Apple's JWKS,
  issuer, audience, expiration, and nonce (with a brief in-memory JWKS cache
  that refreshes on an unknown key ID at most once a minute).
- Accepts configured native app bundle IDs via
  `global.identity_provider.native_client_ids` while keeping the web Services ID
  valid; reuses the normal SSO mapping/registration/reactivation/loginToken
  path.
- Refuses deactivated and locked accounts like the browser SSO callback: both
  checks live in `complete_sso_session`, which both endpoints call.

Note: the Apple `id_token` userinfo fallback that this fork originally carried
was merged upstream, so it is no longer a fork delta.

### 4) `pusher: notify once when streams finish`
Files:
- `src/service/pusher/mod.rs`, `src/service/pusher/send.rs`
- `src/service/pusher/tests.rs`
- `src/mindroom-tests/tests/stream_reply_push.rs`

Behavior:
- Recognizes MindRoom streamed events by the `io.mindroom.stream_status`
  content key (an event-level protocol signal: senders opt their own streamed
  events in; no account privilege involved).
- Suppresses push notifications and unread counts for stream updates in
  non-terminal states, so a streaming agent reply does not notify once per
  chunk. Unknown stream statuses fail closed (suppressed) so a new producer
  state cannot reintroduce push spam.
- When the stream reaches a terminal status (`completed`, `cancelled`,
  `interrupted`, `error`) via an `m.replace` edit, evaluates the surviving
  message as the final content it represents: for `m.room.message` the
  `m.new_content` body, for `m.room.encrypted` the payload without
  `m.relates_to`. Ordinary room/DM/mention/mute push rules then decide the
  single notification instead of the generic edit-suppression rule swallowing
  the terminal update.
- A finished stream counts like a new message: a reply read half-way has not
  been read. For every recipient whose push rules notify on the final
  content, the terminal edit adds the unread count those rules give
  (notification, and highlight when they highlight) to the reply's thread, or
  to the room for a main-timeline reply, and delivers the one push. Whether
  the recipient read the reply while it streamed is not checked, so a receipt
  or the recipient's own send before the stream finished changes nothing. The
  recipient's next receipt clears the count through upstream's normal reset;
  MindRoom's clients send one when the user is looking at the reply as it
  finishes. Upstream resets only on a receipt that advances in the count's own
  context: for a thread reply, a receipt in that thread or an unthreaded one
  (a `"main"` receipt never clears it); for a main-timeline reply, a `"main"`
  or unthreaded receipt. Clients therefore name the terminal edit or a later
  event: another receipt on an event the user already acknowledged, such as
  the reply's first event read mid-stream, clears nothing.
- Suppresses non-notifiable state events (every event carrying a `state_key`
  except `m.room.member` and `m.room.tombstone`), `m.room.redaction` events,
  and `io.mindroom.*` / `com.mindroom.*` timeline control events (except
  initial pending `io.mindroom.tool_approval`), so room-level `RuleKind::Room`
  ("All messages") rules do not notify on internal control traffic or room
  metadata updates while `org.matrix.msc4075.rtc.notification` remains
  untouched.
- Honors `io.mindroom.suppress_push: true` on `m.room.message` (top-level
  `content` or `m.new_content`) and `m.room.encrypted`, suppressing both push
  delivery and unread counts for intermediate agent turns and handoff relays
  (including terminal `m.replace` edits).
- Evaluates initial pending `io.mindroom.tool_approval` timeline events
  (`status == "pending"`, not `m.replace`) against push rules via a synthetic
  `m.room.message` (`msgtype: "m.text"`, `body: ""`, plus `m.mentions` for the
  recipient when `approver_user_id` matches them) while suppressing
  non-matching approvers, non-pending statuses, and replacement edits. The push
  gateway still receives the real `io.mindroom.tool_approval` event and
  content, and threaded approvals increment the thread's unread counts.
- The `push.everything` debug mode honors all of the same suppressions.
- Keeps upstream's recipient-based membership-notification target flag. The
  real gateway fixture checks an invite addressed to someone other than its
  sender, alongside terminal-stream, suppression, and tool-approval
  notification behavior.

### 5) `fix(e2ee): reject replacement of existing device identity keys`
Files:
- `src/api/client/keys/upload_keys.rs`
- `src/service/users/{dehydrated_device,device,keys,mod}.rs`
- `src/mindroom-tests/tests/device_key_immutability.rs`
- `src/main/tests/device_key_read_errors.rs` (upstream fixture, adapted to the
  fork's rejection of identity replacement)

Behavior:
- Treats identity keys for an existing device as immutable in `/keys/upload`:
  the first upload is accepted, an exact-copy re-upload is ignored (upstream's
  nheko workaround, preserving cross-signing signatures), and differing key
  material is rejected with 403 `M_FORBIDDEN` so a client that lost its crypto
  store fails loudly and re-logins instead of becoming a zombie session.
- Only a genuine not-found result permits insertion. Stored-key decoding or
  database-read failures return an error rather than being treated as absence,
  which would bypass the immutability check. This uses upstream's v1.9.1
  read-error handling inside the fork's critical section. Upstream's native
  fixture covers malformed JSON and invalid UTF-8, asserting that rejected
  uploads preserve the stored bytes; the duplicate fork helper is removed.
- A per-device mutex makes the compare-and-insert operation atomic: concurrent
  first uploads choose one identity, exact retries of that identity remain
  successful, and all competing identities are rejected. Device removal uses
  the same mutex and a waiting upload re-checks that the device still exists,
  preventing an in-flight request from resurrecting deleted identity keys.
- `remove_device` deletes the uploaded identity keys so a later login re-using
  the device id can install a fresh identity. Since v1.9.3 that deletion is
  upstream's (`1347197ee`); the fork runs it, and upstream's one-time-key and
  fallback cleanup, within the same per-device lock. Upstream also refuses a
  device ID that names a cross-signing key (`99c6c320a`).
- Upstream main serializes one-time-key claims per device with its own
  `claiming_one_time_keys` lock (`25dbbb5a3`). No path takes both locks: claims
  take only upstream's, and device removal clears one-time keys under only the
  fork's.

## Operational Changes

### 6) `ci: fork release automation, container publishing, and GitHub checks`
Files:
- `.github/workflows/mindroom-release.yml`, `.github/workflows/auto-mindroom-release.yml`
- `.github/workflows/mindroom-container-release.yml`, `.github/workflows/mindroom-ci.yml`
- `scripts/fork_release_tag.py`, `docker/bake.sh`

Behavior:
- Computes `v<base_version>-mindroom.<n>` tags on `main`, creates/reuses the
  matching GitHub Release, publishes Linux `x86_64`/`aarch64` binaries, and
  dispatches container publication. Runs the fork's own GitHub-hosted checks.
- Pins CI rustfmt to `nightly-2026-11-20`, matching upstream `f7dc0770b`'s
  locked Fenix nightly formatter (`rustfmt 1.12.0-nightly 6905905bb7
  2026-11-19`), which passes both `f7dc0770b` and all fork commits; upstream CI
  floats `nightly`. Keep the two CI formatter commands aligned.
- Frees about 30 GB on the GitHub runner before building (preinstalled .NET,
  Android, GHC and CodeQL), because the v2.0.0 debug test build and the
  release build together no longer fit in its free disk.
- Runs tests with `TMPDIR` set to the CI runner's temporary directory. This
  environment setting remains in the CI owner after dropping the full-state
  sync fix now supplied by upstream.

## Tests

Fork integration tests live in the `mindroom-tests` crate
(`src/mindroom-tests/`). They pin the rebase-sensitive fork behaviors: SSO/UIAA,
native Apple, deactivation/erase, Synapse-admin deactivation reason and room
departure, edit-purge/bundling composition, the orphaned long-text sidecar
sweep command and its retry after a storage failure
(`orphaned_sidecar_sweep.rs`, `orphaned_sidecar_delete_retry.rs`),
device-key immutability/cleanup/
concurrency, real gateway stream/invite notifications and finished-stream
unread counts, and audience-bound
OpenID tokens with the option on and off (`openid_audience.rs`,
`openid_audience_disabled.rs`), OpenID userinfo room membership
(`openid_room_membership.rs`), deactivated accounts left out of the user
directory with `show_all_local_users_in_user_directory` on and off
(`user_directory_deactivated.rs`, `user_directory_deactivated_rooms.rs`),
delayed events limited to the sender's own MatrixRTC call leave
(`delayed_call_membership.rs`), and conflicts with a finished delayed event
answered as missing on the unstable MSC4140 paths
(`delayed_finished_not_found.rs`).
Stream classification also has unit tests in `src/service/pusher/tests.rs`.

The v1.9.3 rebase adds pins for its semantic overlaps:
`uiaa_sso_policy.rs` (the advertised UIAA flows per account origin and
credential, the legacy-origin repair, and the JWT refusal for SSO accounts),
`sync_edit_compaction.rs` (legacy and sliding `/sync` compaction, initial and
incremental, keeping a state event that claims a replacement), a
`loginToken` exchange after SSO self-reactivation in
`sso_callback_completion.rs`, since login now refuses deactivated accounts, and
a race of identity uploads against device removals in
`device_key_immutability.rs`, which takes the fork's per-device lock and then
upstream's per-user device-list lock and must neither stall nor leave keys
behind.

Unit tests that load `Config` from a file also read `TUWUNEL_*` environment
overrides. The development shell exports `TUWUNEL_DATABASE_PATH`; unset it when
running the suite, or those tests share one database and fail.

Database-path isolation, pagination bounds, quiet-room full-state sync, and
stored-key corruption coverage use upstream's native tests under `src/main/tests/`.
Only the corruption fixture's normal replacement/retry expectations are adapted
to the fork's immutable-device policy; all corrupt-byte cases remain intact.

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
# rooms.relations.bundle_edits defaults to true in the fork; set false only to opt out.
```

### OpenID audience binding
```toml
[global]
# Off by default; enable only after backends and clients send audiences.
mindroom_openid_audience = true
```

### Custom ephemeral events (MSC2477)
```toml
[global]
# On by default; false refuses sends and stops advertising the feature.
mindroom_ephemeral_events_enabled = true
```

### Accounts assigned to SSO sign-ins
```toml
[[global.identity_provider]]
# Empty by default; list only addresses your organization assigns, for
# accounts created for those people, and remove entries once they signed in.
assigned_accounts = { "alice@example.com" = "@alice:example.com" }
# Empty by default; claims every sign-in of a listed address must hold. Set it
# before the app admits accounts outside the organization.
assigned_accounts_claims = { hd = "example.com" }
# Unset by default; the ID of an appservice whose accounts, registered with
# m.login.application_service, their people claim at their first sign-in.
# Needs userid_claims and is refused with trusted = true.
userid_claims = ["email"]
claim_accounts_of_appservice = "import"
```

### Registration rules for SSO sign-ins
```toml
[[global.identity_provider]]
# Empty by default: registration works as before. With rules, only a sign-in
# meeting one creates an account, which joins that rule's rooms. An alias
# counts only as its room's canonical alias; a room ID always does.
registration_rules = [
  { claims = { hd = "example.com" }, rooms = ["#lobby:example.com"] },
  # external: joins only rooms it is invited to, creates and invites nothing.
  { claims = { email = "alice@school.example" }, rooms = ["#class:example.com"], external = true },
]
```

### Rooms external accounts are kept out of
```toml
[global]
# Empty by default. Room IDs or aliases of this server; external accounts are
# neither invited to nor joined to them.
external_forbidden_rooms = ["#staff:example.com"]
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
- Superseded edits can be permanently removed when purge is enabled; bundles
  let clients recover the surviving edit from history responses. They cannot
  restore purged revisions if the newest edit is later redacted.
- Admin-deactivated SSO accounts stay deactivated on future login attempts.
- An SSO-origin account that also holds a real password is offered both
  `m.login.password` and `m.login.sso` in UIAA (upstream's rule since v1.9.3);
  before v1.9.3 the fork offered such accounts SSO only.
- Native Apple login requires the app bundle ID in `native_client_ids`.
- An image without registration rules ignores `registration_rules` (only the
  rule's own fields are `deny_unknown_fields`, so the config still loads) and
  the stored external mark, so downgrading lifts the confinement of external
  accounts.
- A client that lost its crypto store but kept its access token receives 403
  `M_FORBIDDEN` on `/keys/upload` until it logs in again (device identity keys
  are immutable per device id).
- Non-terminal `io.mindroom.stream_status` events neither push nor count as
  unread; terminal events use ordinary recipient push rules and count like a
  new message, even for a recipient who read the reply while it streamed. The
  classifier does not deduplicate multiple distinct terminal events for the
  same stream. Deploy it together with clients that send a receipt on the
  terminal edit when the user watches it finish: an older client acknowledges
  the reply's first event again, which does not advance, so a reply the user
  watched keeps its count until the next event in its thread.
- OpenID audience binding is off unless `mindroom_openid_audience = true`.
  With it on, tokens requested without `io.mindroom.audience` stay unbound and keep verifying at userinfo calls without the query audience, so Element Call, LiveKit, the hosted provisioning service, and stock relying parties work unchanged.
  A relying party that sends `io.mindroom.audience` to userinfo accepts only tokens bound to that audience, so its clients must request bound tokens.
  Conversely, a client that requests a bound token can only use it at relying parties that send the matching audience; relying parties that send none get 401.
  Enable the option only after MindRoom backends and MindRoom Chat clients, including the iOS app, send audiences.
- Upstream v1.9.1 `/messages` treats `from` and `to` as directional
  stream-position bounds and
  rejects malformed pagination tokens with `M_INVALID_PARAM`.

### Fresh-device thread titles and conversational previews

The shared read-time thread bundler adds two optional fields beside the
standard `unsigned.m.relations.m.thread` fields, so a new device can list
threads with their titles and real last replies without opening each one:

- `io.mindroom.title`: the newest title event in the thread.
- `io.mindroom.latest_reply`: the newest direct thread event that is not a
  title.

Both are full client events without `room_id`, like `latest_event`. With
`rooms.relations.bundle_edits` on, each carries its newest valid `m.replace`
edit (same sender and type, not redacted) that the requester may see; a
rejected newer edit falls back to an earlier valid one. `latest_event`, `count`
and `current_user_participated` are unchanged. The fields reach `/messages`,
`/context`, `/event`, `/relations`, `/threads` and the v3 and sliding sync
timelines through the shared bundler. Encrypted rooms receive neither field.

A title is a direct, non-state, unencrypted `m.thread` event with
`msgtype: m.notice` and an `io.mindroom.thread_summary` that JavaScript treats
as true (an object, the older `true`, a non-empty string, a non-zero number or
an array), from any sender, as MindRoom and its clients read titles: agents
retitle threads they never replied in, and any member can rename a thread.
Every other direct thread event is a conversational reply, including an
`m.text` with a marker and a notice whose marker is `false`, `0`, `""` or
`null`. A marker inside an edit's `m.new_content` never makes a title:
clients read the title only from `io.mindroom.title`.

Reads skip redacted, ignored-user, forbidden-server and erased events and
enforce the requester's history visibility for each candidate and each edit.
Newly embedded copies omit stored `m.relations` (upstream `0dbe24238` and
`7752984d2` cover `/initialSync`, `/notifications`, `/events`, `/state`,
`/search`, and `m.thread.latest_event`; the fork's `#183` remainder strips
stored `m.relations` on embedded `m.replace` edits and `#143` previews via
upstream's `served_unfolded_to`/`remove_aggregations`). Unreadable unsigned data is
omitted from the copy; fresh, requester-checked bundles may be added afterwards.
A redacted root keeps previews of its live children.

Cost: each served root reads its native relation rows (every event relating
to the root: thread events, reactions, edits of the root, rich replies)
newest first and stops as soon as both fields are found, reading at most
**50 rows**, plus at most 50 edit rows per selected preview. A field whose
event is not within those 50 rows is omitted; clients fall back to opening
the thread. There is no preview index, migration, purge hook or
configuration option.

Live relations to backfilled events now enter the native relation index
(`add_relation`), so a backfilled root's live replies are found. Such
relations received before this change were never stored and stay absent.
This also reaches other readers of that index: a duplicate reaction to a
backfilled event is rejected, and `related_by_*` filters see live relations
to backfilled events. `/relations` still answers an empty page for a
backfilled target (`src/api/client/relations.rs`).

### Strict host-vs-scheme matching in `redirect_allowlisted`

`tuwunel_core::utils::url::redirect_allowlisted` (used by OIDC dynamic client
registration, OIDC client-approval waiver, and legacy SSO's Continue sign-in
bypass) no longer conflates hostnames and private-use URI schemes or ignores
the URI transport scheme:

- Bare entries in `authentication.oidc.registration_allowed_redirect_hosts`
  (legacy `oidc_registration_allowed_redirect_hosts`), such as `element.io`,
  `localhost`, `127.0.0.1`, or `::1` / `[::1]`, match only `https` URIs with
  that exact host or loopback `http` URIs (with no userinfo). They never match
  hostless custom schemes (such as `element.io:/callback`), non-loopback
  plaintext `http`, or arbitrary custom schemes carrying that host.
- Colon-suffixed entries (such as `io.element.android:`) match only hostless
  RFC 8252 §7.1 private-use URIs with no authority whose scheme contains a dot
  (`io.element.android:/callback`). They never match `https` hosts of the same
  name or URIs with an authority (`io.element.android://callback`).
- Deployments that previously listed a bare private-use scheme must replace it
  with the colon-suffixed form (do not keep both, or the bare entry still
  trusts that `https` host).
