# MindRoom Tuwunel Fork - Changes vs Upstream

This document describes the current MindRoom fork behavior on top of upstream
`tuwunel`. The fork is rebased directly onto upstream commits; see
`docs/rebase-*.md` for the per-rebase log. As of the 2026-10-11 rebase the base
is the upstream `v1.9.4` release (`f473d9a4a`). Earlier changes merged upstream
and dropped from the fork include the room-creation default power-level
override and the SSO grant-cookie path hardening (2026-07-07 rebase), and the
drained/zero one-time-key counts fix and public MatrixRTC transport discovery
(2026-07-21 rebase, upstream `164b8da61`/`007033cd5` and `a1776c368`). The
Sliding Sync `num_live` correction was dropped at v1.9.0 because upstream now
contains the same implementation and tests (`6acc3a7d1`, `942dd66e3`, and
`a508c7331`).

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
does not contain, in merge order (#15 to #105), with the v1.9.3 and upstream
main rebase records after #64 and this rebase's documentation last. That
rebase carried no upstream backports; upstream `750dbc0b9` from `main` is
carried since (see "Former members do not list a room's aliases").
See the [current ownership and rebase procedure](docs/rebase-v1.9.4-2026-10-11.md).
Earlier rebase records remain historical snapshots.

## Runtime Changes

### Stored signing keys of a remote server stay small

Every signing-key document fetched for a server, from the server itself or
from a notary, was merged into the one row stored for that server, and nothing
removed keys from it. A server that answered each fetch with new key ids grew
that row without bound, and every key lookup for the server parsed the whole
row, including the lookup for each incoming federation request it signs. A
merge that would make the row larger than 64 KiB now keeps only the fetched
keys, and a fetched document over 64 KiB on its own is not stored. A stored row
over 64 KiB, left from before this change, is treated as absent: lookups do not
parse it, and the next fetch for the server replaces it. Servers publish a few
keys in a few hundred bytes. Upstream has the same bug. Files:
`src/service/server_keys/mod.rs`; test in `src/service/server_keys/tests.rs`.

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
`max_response_size`. Upstream has the same bug. Files:
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

### Sliding sync looks up delivered required state selectors in a set

When a room's sliding sync configuration changes, each required state entry is
checked against the selectors the room was last delivered with, so state the
client had not asked for before is sent in full. Each check scanned the stored
list, so the cost grew with the product of the current entries and the stored
selectors, and a request with a million selectors kept a worker busy for
minutes. The stored selectors are now collected into a hash set once per room.
Upstream has the same bug. File: `src/api/client/sync/v5/rooms.rs`; test in
`src/api/client/sync/v5/rooms/tests.rs`.

### Sliding sync caps the required state selectors

The `required_state` list of a sliding sync list or room subscription had no
length limit. Each request copied, hashed and, on a room's initial pass, looked
up every selector for every room in the window, and the connection kept a
fingerprint of each selector per room and was stored after every response, so
the cost grew with the rooms times the selectors. The connection now keeps only
the first 256 selectors of each list and subscription, and a room uses at most
256 selectors across the lists and the subscription that cover it. Upstream has
the same bug. Files: `src/service/sync/mod.rs`,
`src/api/client/sync/v5/rooms.rs`; tests in `src/service/sync/tests.rs` and
`src/api/client/sync/v5/rooms/tests.rs`.

### Sliding sync caps the lists, ranges and extension filters

A sliding sync connection kept every list a request named with all of its
ranges, and the account data, receipts, typing and profiles extensions kept
their `lists` and `rooms` filters as sent. Each pass matched every room in the
window against every list for each range, and every room an extension filter
named against every list ID of that filter, without yielding, so a request
with many lists, ranges or filter entries kept a worker busy for minutes. A
connection now keeps at most 64 lists and the first 16 ranges of each, and an
extension filter keeps its first 64 list IDs and its first 256 rooms. Upstream
has the same bug. File: `src/service/sync/mod.rs`; tests in
`src/service/sync/tests.rs`.

### Sliding sync caps list IDs and list filters

A sliding sync list ID had no length limit, and a list kept its `spaces`,
`tags`, `not_tags`, `room_types` and `not_room_types` filters as sent. Each
pass copied a room with the IDs of the lists it matched for every range it was
selected in, and matched every room against every filter entry, where each
`spaces` entry read the whole state of the named room. A connection now ignores
lists with an ID longer than 64 bytes, the limit Synapse documents, and drops
such IDs from extension filters. Each list keeps the first 16 distinct entries
of each filter, and a `spaces` entry looks up the one `m.space.child` event
naming the room. Upstream has the same bug. Files: `src/service/sync/mod.rs`,
`src/api/client/sync/v5/filter.rs`; tests in `src/service/sync/tests.rs` and
`src/main/tests/sync_v5_list_filters.rs`.

### Sliding sync copies each window room once

Each pass copied a room, with the IDs of the lists it matched, once for every
list and range that selected it, and a list kept a range repeated in a request
as often as it was sent. The window holds each room once, so 64 lists of 16
identical full ranges copied every room about a thousand times in a pass that
does not yield, and kept a worker busy for seconds with a thousand rooms. A
list now keeps each distinct range once, and the window copies a room the
first time a range selects it. Upstream has the same bug. Files:
`src/service/sync/mod.rs`, `src/api/client/sync/v5/selector.rs`; test in
`src/service/sync/tests.rs`.

### Sliding sync long polls wait when only fallback key types are new

The e2ee extension reports the device's unused fallback key types on every
pass, including an empty list, and a response holding them counted as
non-empty. Whenever anything on the server had moved the global count since
the client's position, the next long poll answered at once with only those
types, so a client with the e2ee extension on a busy server polled
continuously instead of waiting. MindRoom's agents each committed a durable
sync batch for every such answer: about 2,300 of 3,000 SQLite fsyncs a minute
on an idle MindRoom deployment. A pass now ignores the fallback key types when
deciding whether it found anything. Types already collected stay in the
response, including when the poll times out. Upstream has the same bug, from
MSC2732 fallback key support (`bfc1ccaba`). File: `src/api/client/sync/v5.rs`;
tests in the same file and in `src/main/tests/sync_v5_fallback_key_wakeups.rs`.

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
`src/api/client/{dehydrated_device.rs,keys/upload_keys.rs,keys/upload_signing_keys.rs}`,
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
`src/api/server/send.rs`, `src/api/client/read_marker/mod.rs`; test in
`src/main/tests/federation_receipt_edu.rs`.

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

### Left rooms carry state only for users who joined

A user who withdrew a knock or rejected an invite has the room among their left
rooms, and legacy `/sync` sent its whole state at the leave: every member,
power levels, topic and any custom state, although the user was never in the
room. A left room's state now goes only to a user who once joined it, or when
the room is world-readable; anyone else gets only their own membership.
Upstream has the same bug. Files: `src/api/client/sync/v3.rs`; test in
`src/main/tests/sync_left_room_state.rs`.

### Sync timelines follow history visibility

Legacy `/sync` and sliding sync returned a room's newest events without
checking history visibility, so a user who joined a `joined` or `invited` room
received events sent before their join or invite, and a room left by
rejecting an invite to a `shared` room carried its recent messages. Timeline
events now pass the per-event check `/messages` uses, except that the user's
own leave or ban is always kept. The `state` section only covers changes
before the first timeline event, so the timeline starts after the last hidden
state event (such as a topic change while the user was away), and is `limited`
when that drops visible events. Upstream has the same bug. Files:
`src/api/client/sync/mod.rs`; test in
`src/main/tests/sync_history_visibility.rs`.

### send_join response events are checked before they are stored

Joining a room over federation stored every event of the `send_join` response
as an outlier, replacing any copy this server already had. An event whose
content no longer matched its content hash was stored as received instead of
redacted, and its `unsigned` data was kept. Such events are now redacted and
`unsigned` is dropped. An event in this server's timeline keeps its stored copy,
and so does any stored event whose new copy had to be redacted; a copy that
matches its hash still replaces an outlier, such as the unchecked knock state
stored before the join. Upstream checks that `auth_chain` events belong to the
joined room since v1.9.4 (`d3db90b1d`) and has the rest of the bug. Files:
`src/service/server_keys/verify.rs`, `src/service/membership/join.rs`; test in
`src/service/membership/join/tests.rs`.

### send_join response events pass the authorization rules

Joining a room over federation installed the `state` and `auth_chain` of the
`send_join` response after checking only their hashes, signatures and format;
only the join event itself went through the authorization rules. A state event
the rules reject, such as a membership event for another user of this server,
or a create event that cannot belong to the room ID, became part of this
server's room state. Each event of the response is now checked against its own
auth events in depth order, and is accepted only if all of its auth events were
accepted before it in the same response; a copy stored earlier, such as knock
state, does not count. An event that is not accepted is not stored and stays
out of the room state. A rejected create event, or one whose room version
differs from the `make_join` answer, fails the join.
Upstream has the same bug. Files: `src/service/membership/join.rs`,
`src/service/rooms/event_handler/handle_outlier_pdu.rs`; test in
`src/service/membership/join/tests.rs`.

### send_join state keeps other local users' memberships

Forcing the `send_join` state replays each `m.room.member` event in it, so a
join the authorization rules accept for another user of this server marked
that user as joined. Up to room version 10 such a join passes from any sender
when its only previous event is a create naming that user as creator, and an
old join of a user who has since left passes too. A join of a local user other
than the joining user is still stored, as other events may name it, but is now
left out of the room state, as member events are on the knock path. The forced
state also replaced the room's, so a local user the answer left out lost their
leave from it while still recorded as left, and a later event whose state had
their old join joined them again. The member event the room's state has for a
local user other than the joining user now stays, unless the answer has later
ones for them that follow it through their auth events, such as an unban and
then a new invite. Knock state, which leaves member events out, likewise keeps
the member events the room's state has for other local users. A join of a local
user that the user did not send is refused before any of this, as described
under "A local user's join is sent by that user". Upstream has the same bug.
Files: `src/service/membership/{join.rs,knock.rs}`; tests in
`src/service/membership/{join,knock}/tests.rs`.

### send_join answers an old join only while its user is joined

`send_join` answered with the room's current state and auth chain for a join
event this server already had, without looking at it again. A server whose
user had once joined could send that join again after the user left, was
kicked or was banned, and read the room's current state each time. A join this
server already has is now answered only while its user is still joined, as a
retry of a join that still stands, and refused otherwise. New joins are
unchanged. Upstream has the same bug. File: `src/api/server/send_join.rs`;
test in `src/main/tests/federation_membership_refusal.rs`.

### A restricted join refused here is not authorised by a local user

When a user's join to a restricted room this server is in fails the allow list
here, the join asks a server from `via` for a `make_join` template and signs
it. The template's `join_authorised_via_users_server` was kept even when it
named a joined user of this server, so this server's signature authorised the
join it had just refused, and the remote server could complete it. The join
now fails with the local refusal when the template names a local authoriser;
a template naming the remote server's own user is still signed and sent.
Upstream has the same bug. File: `src/service/membership/join.rs`; test in
`src/main/tests/restricted_join_fallback.rs`.

### The state before an incoming event comes only from its room

When the state at an incoming PDU's prev events is not known, it is built
locally from the events we hold, or taken from the sending server's
`/state_ids` answer. Both look events up by id alone: the local build followed
a held outlier's prev events into another room, and a `/state_ids` answer
could name events we hold from another room. The PDU was then authorized
against and stored with that other room's state. Both now check the room of
every event they load: `/state_ids` rejects such a PDU, and the local build
falls back to `/state_ids`. Upstream checks the state taken from the prev
events themselves since v1.9.4 (`94198c340`) and has the rest of the bug.
Files: `src/service/rooms/event_handler/{fetch_state,state_local_build}.rs`;
test in `src/main/tests/federation_prev_event_room.rs`.

### A room keeps its one create event

In room versions 1 to 11, an `m.room.create` event for a room this server is
in, other than the room's own create, passed the authorization rules when it
came from the server named in the room ID, since a create event has no rules
that depend on the room state. It was added to the timeline and resolved into
the current state, where it could take the place of the room's create. It was
also stored as an outlier when it arrived in a transaction, as a fetched auth
event or in a `/state_ids` answer, and events naming it as an auth event were
authorized against it, so state resolution could apply them. Such a create is
now refused before it is stored, and so is an event naming another create as
an auth event, even when that create was stored earlier. The authorization
rules are unchanged. Upstream has the same bug.
Files: `src/service/rooms/event_handler/{handle_outlier_pdu,upgrade_outlier_pdu}.rs`;
test in `src/main/tests/federation_second_create.rs`.

### A local user's join is sent by that user

The authorization rules accept a join from any sender when its only previous
event is a create and its state key is that create's creator: the create's
`creator` up to room version 10, its sender since room version 11. A remote
event needs only the signature of its sender's server, so another server could
send a join of a user of this server, through the room's own create or a second
one, and have it stored and applied to the room's state, which recorded the
user as joined. This server makes the joins of its own users, each sent by the
user it joins, so a join of a local user sent by anyone else is now refused
wherever a remote event is checked against its auth events: in transactions,
fetched events, `/state_ids` answers, backfill and `send_join` answers. The
authorization rules are unchanged, and other servers' users are not affected.
Upstream has the same bug.
File: `src/service/rooms/event_handler/handle_outlier_pdu.rs`; tests in
`src/main/tests/federation_local_join.rs` and
`src/service/membership/join/tests.rs`.

### Rooms created in the same millisecond get their own room IDs

A room version 12 room's ID is the hash of its create event, which holds only
the sender, the content and a millisecond timestamp. One user's `createRoom`
requests with the same create content in the same millisecond got one room ID,
and the later create was appended to the earlier room, resetting its state.
Either request's next events could then fail authorization (``sender's
membership `leave` is not `join` ``, ``no `m.room.join_rules` event``), or both
requests answered with the same room. MindRoom's router creates its managed
rooms four at a time at startup, and two of three starts of a 22-room
deployment lost a room this way. A new version 12 room's create event now moves
to a later millisecond while its room ID belongs to an existing room. Upstream
has the same bug, latent since `628597c31` and exposed when `767f53cf5` made 12
the default room version. File: `src/service/rooms/timeline/create.rs`; test in
`src/main/tests/create_room_same_millisecond.rs`.

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

Fetching the missing auth events of an incoming event walked the remote server's
auth chain one event at a time and kept every fetched event in memory until the
walk ended, with no limit on the number of events and only the federation
response limit (256 MiB by default) on each one. A walk now gives up and drops
what it fetched once it holds `max_fetch_prev_events` events (default 1024) and
would fetch another. The walks for one event's auth events, or for the events of
one `/state_ids` answer, keep what they fetched until all of them end, so they
also share one limit of `max_fetch_prev_events` times 64 KiB (64 MiB by default)
on the events they fetch; a walk whose fetched event would pass it gives up the
same way. A walk keeps each fetched event as canonical JSON without its
`unsigned` field, which the outlier path removes before its own size check, and
parses it again only to authorize it; an event that is then still larger than
the 65,535 byte PDU limit is a failed fetch. The fetcher rejects an event
response larger than four times that limit before parsing it, which leaves room
for the `unsigned` data a server serves with the event. Upstream has the same
bug. Files: `src/service/rooms/event_handler/fetch_auth.rs`,
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
events against `max_fetch_prev_events`. Every queued event is fetched in the
end, so one incoming event could start about 20 times that many fetches. The
walk now counts every event it queues, and runs at most 32 of their fetches at
once (see the next entry). The `/get_missing_events` batch
fetched before the walk asks for 10 events but stored every event the remote
server answered with; it now stores at most 10. Each auth chain fetch started
for the incoming event, for its own auth events, for each prev event the walk
fetches and for each event of that batch, also had its own limit of
`max_fetch_prev_events` times 64 KiB; they now share one. Upstream has the same
bugs. Files: `src/service/rooms/event_handler/fetch_prev.rs`,
`src/service/rooms/event_handler/fetch_auth.rs`,
`src/service/rooms/event_handler/handle_incoming_pdu.rs`; tests in
`src/main/tests/auth_chain_fetch_budget.rs`.

### A prev event walk runs a few fetches at once

The walk over an incoming event's missing prev events started every fetch it
queued at once, up to `max_fetch_prev_events` (default 1024). The fetcher runs
at most 256 fetches for all rooms together, so one walk over a wide gap whose
server answered slowly could take all of them, and fetches for every other
room waited behind it. A walk now runs at most 32 of its fetches at once, more
than the 20 prev events one event may name, and starts the rest as those
finish, so a wide gap still fills completely. Upstream has the same bug. File:
`src/service/rooms/event_handler/fetch_prev.rs`; test in
`src/main/tests/auth_chain_fetch_budget.rs`.

### Federation bodies and incoming PDUs are parsed after their checks

A federation request's body was parsed into a JSON tree before the request was
authenticated, and the signature check copied that tree and then waited for the
origin's signing key, which is fetched from the origin when it is not cached. A
body can be up to `max_request_size` (24 MiB by default) and its tree many times
that, so a slow key answer kept two such trees per request until the request
timed out, and a handled request kept its tree until the handler finished,
although no federation handler reads it. The PDUs of a `/send` transaction, an
invite's event and the create event in its stripped state, and the events of
`send_join`, `send_leave` and `send_knock` were parsed at any length, since the
64 KiB PDU limit is checked only later, and a transaction parsed all of its PDUs
before any of them waited for its room's turn. The origin's key is now resolved
before the body is parsed, and the signature check uses that key rather than
looking it up again, which would fetch a key document too large to store a
second time. A federation request drops the parsed body once its typed request
is built, and an incoming PDU longer than four times the PDU limit, the limit
the fetcher already applies to fetched events, is refused before it is parsed.
A transaction keeps each PDU's JSON raw and parses it again when its room's turn
comes. Upstream has the same bug. Files:
`src/api/router/{args.rs,auth.rs,auth/dispatch.rs,auth/server.rs}`,
`src/api/server/{send.rs,invite.rs,send_join.rs,send_leave.rs,send_knock.rs}`,
`src/core/matrix/{pdu.rs,pdu/format/check.rs}`,
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
the same bug. Files: `src/api/router/args.rs`,
`src/api/client/admin/mas/mod.rs`; test in
`src/main/tests/client_body_after_auth.rs`.

### Missing-event and state id answers are read up to a limit

An incoming event whose prev events are missing asks the sending server for
them with `/get_missing_events`, and the state fallback asks it for the state
at an event with `/state_ids`. Both answers were read under the generic
federation response limit (256 MiB by default), and each event of a
missing-events answer was parsed in full before anything checked its size,
although the request asks for only 10 events. A missing-events answer is now
read up to the size of one `/event` answer for each event asked for (about
2.5 MiB for 10), and an event in it larger than four times the PDU size limit
is dropped before it is parsed. A `/state_ids` answer is read up to 64 MiB,
the ids of over a million events. A larger answer is dropped while it is read,
and neither limit exceeds `max_response_size`. Upstream has the same bug.
Files: `src/service/fetcher/{mod.rs,transport.rs,validate.rs}`,
`src/service/rooms/event_handler/fetch_prev.rs`; test in
`src/main/tests/federation_event_response_limit.rs`.

### A `/state_ids` answer walks each event once

When the state before an incoming event is taken from the sending server's
`/state_ids` answer, every id in the answer started a walk, and an id named
more than once started one for each copy. A walk for an event we hold loads
the whole event, outside the size limit the walks share for fetched events, and
all walks keep their results until the last one ends; only then was the second
copy refused as a repeated state key. An answer of up to 64 MiB can name one
large stored event over a million times. Each event of the answer is now walked
once, and an answer naming an event more than once gives the state it would
give naming it once. Upstream has the same bug. File:
`src/service/rooms/event_handler/fetch_state.rs`; test in
`src/main/tests/auth_chain_fetch_budget.rs`.

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

### SSO sign-in skips usernames reserved by an appservice

A new SSO account took its username from the identity's claims, such as
`preferred_username` or the email's local part, without checking appservice
namespaces, while `/register` and native OIDC registration refuse a username
an appservice reserves exclusively. Signing in with an email like
`whatsapp_<number>@example.com` on a server with public SSO and a WhatsApp
bridge registered the bridge's ghost user ID, and the bridge later took that
account as its own ghost. A claim inside an exclusive namespace is now skipped
like a forbidden username, so the next claim or the identity's fallback
username is used. Upstream has the same bug. File:
`src/api/client/session/sso.rs`; test in
`src/main/tests/sso_fallback_account.rs`.

### Knock state is checked before it is stored

A knock on a room this server is not in stored every non-member
`knock_room_state` event as an outlier under the event ID computed from it,
unchecked, replacing any copy this server already had. A copy whose content no
longer matched its hash kept the real event ID, so it could replace a stored
event of any room, and events of other rooms were stored too. Knock state events
are now checked as `send_join` events are: only a non-member full PDU of the
knocked room whose signatures and content hash check out is stored, if this
server has no copy yet, and enters the room's state. The knock state the client
sees is built from that room state, so it now leaves out events that fail the
check. Upstream has the same bug. Files: `src/service/membership/knock.rs`;
test in `src/service/membership/knock/tests.rs`.

### Sliding sync receipts and typing only for joined rooms

Simplified sliding sync sent other users' read receipts and typing for every
room in the window: rooms the user has left or been removed from, which a room
subscription keeps there, and rooms they are invited to or have knocked on.
Both now follow the required-state rule, so only joined rooms, and
world-readable rooms peeked without a membership, carry them, as v3 `/sync`
sends ephemeral events only for joined rooms. The user's own private read
receipt and room account data are unchanged. Upstream has the same bug. Files:
`src/api/client/sync/v5/{range.rs,rooms.rs,extensions/typing.rs}`; test in
`src/main/tests/sync_v5_departed_ephemeral.rs`.

### Former members read the room state from when they left

`/rooms/{roomId}/state`, `/state/{eventType}/{stateKey}` and `/members` admit
a former member under `shared` history visibility, but answered from the
room's current state, so a user who had left or been kicked or banned kept
seeing later renames, topics, power levels and new members. A former member
now reads the state as of their leave or ban, as the spec requires and as
`/initialSync` already did. Knocking again, or withdrawing that knock, does not
move this point: these routes, `/initialSync` and a left room in `/sync` serve
the state after the leave or ban that ended the user's last join, found by the
same walk that bounds their history. Upstream has the same bug. Files:
`src/service/rooms/state_accessor/user_can.rs`,
`src/api/client/{state.rs,membership/members.rs,room/initial_sync.rs,sync/v3.rs}`;
tests in `src/main/tests/{state_departed_member.rs,knock_withdrawal_history.rs}`.

### The room summary is not given to former members

The room summary of an invite-only or restricted room admitted a former member
under `shared` history visibility and answered with the room's current name,
topic, avatar, alias, join rule and member count, so a user who had left or
been kicked or banned kept seeing later changes. Such a room's summary now goes
to a joined or invited user, or to anyone while the room is world-readable, as
in Synapse; the restricted-room and guest checks are unchanged, and an invitee
is now admitted under every history visibility. Upstream has the same bug.
File: `src/api/client/room/summary.rs`; test in
`src/main/tests/state_departed_member.rs`.

### Former members do not list a room's aliases

`GET /rooms/{roomId}/aliases` admitted a former member under `shared` history
visibility, so a user who had left or been kicked or banned kept listing the
room's current local aliases. The aliases now go only to a joined user, or to
anyone while the room is world-readable. The check is upstream `750dbc0b9`
from `main`, carried as a cherry-pick until the fork's base contains it. File:
`src/api/client/room/aliases.rs`; test in
`src/main/tests/state_departed_member.rs`.

### `/context` keeps current state from requesters who may not read it

A room's create event and backfilled events have no state snapshot, so
`/context` around one of them returned the room's current state, even to a user
who had never joined. That fallback now applies only to a requester who passes
the `/state` check (`user_can_see_state_events`) or to the admin room-context
endpoint; anyone else gets an empty `state`. A former member gets the state
from when they left, as `/state` serves them. Upstream has the same bug. Files:
`src/api/client/context.rs`; test in
`src/main/tests/context_snapshotless_state.rs`.

### Bundled aggregations follow the requester's visibility

A served event's bundled `m.thread` summary and `m.replace` edits were added
without checking whether the requester may see them, so a user who had left or
been removed from a room still got a thread's newest reply and the newest edit
of an event they could read, even when those were sent after they left. A
requester who is no longer in the room now gets an edit only when the room's
history visibility lets them see it, and no thread summary when it hides the
latest reply. `/rooms/{roomId}/initialSync` and `/notifications`, which served
the stored event without this step, now apply it as well. Upstream has the same
bug.
Files: `src/service/rooms/pdu_metadata/bundling.rs`,
`src/api/client/{room/initial_sync.rs,push/notifications.rs}`; test in
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

### Recursive `/relations` pages give each event once, in order

With `recurse=true`, the walk began reading an event's own relations only once
it had served that event, and it merged the per-event reads in whatever order
they answered, so a page was not in position order. The next page applied the
`next_batch` of the last served event to every level of the walk: an edit or
reaction whose parent was on an earlier page was never served, and a page
ending on an edit served replies again. Paginating `dir=b` through a thread of
about 220 replies (419 events), pages of 30 missed 63 events and pages of 10
missed 47 and repeated 56; every lost event was below a direct reply.

The walk now reads the positions of the whole tree from the relation rows, to
the same depth (the target's relations and three levels below them), before it
loads any event, keeping only rows whose event is stored in the room: rows left
by edits the purge deleted and rows from events in other rooms are neither
served nor walked, as before. It then applies `from`, orders the positions in
the requested direction, stops at `to` (at the first event past it, whether or
not `to` names one of them), and loads events only until the page is full, so
every event is served once and in position order, in both directions. An event
relating to two events of the tree is kept once, at its shallowest depth, and
counts once against the limits. `recursion_depth` is the
depth the walk reached, the same on every page. A walk keeps at most 32,768
events and reads at most 262,144 relation rows, unservable ones included. A
level that does not fit is dropped whole, with the levels below it, and
`recursion_depth` says how deep the walk went; if the target's own relations
do not fit, they are served as without recursion, at depth 0. The decision
does not depend on `from`, so every page of a walk serves the same levels.
A backfilled target gets an empty page with `recursion_depth`.
A backfilled `from` (zero or negative, as `/messages` hands out) comes before
every relation, since relations are only indexed for normal events: forward
pages start at the first relation and backward pages are empty, with or
without `recurse`. The non-recursive read compared it as an unsigned key, so
its forward pages were empty, its backward pages returned every relation, and
a `from` of 0 hit a debug assertion. Otherwise nothing changes without
`recurse`. Upstream has the same bugs. Files:
`src/api/client/relations.rs`,
`src/service/rooms/pdu_metadata/{mod.rs,relations.rs}`; tests in
`src/main/tests/relations_recurse_order.rs` and
`src/service/rooms/pdu_metadata/tests.rs`.

### A device's tokens rotate under its device lock

Refresh-token rotation read the token a device points at, removed it, and wrote
the new one without a lock, so two concurrent refreshes of one token could each
write a refresh token while the device kept pointing at only one. Logout and
device removal delete only the token the device points at, so the other one
stayed. A refresh already in flight could also issue tokens to a device removed
meanwhile. `set_access_token` now takes the per-device lock that
`remove_device` already holds and refuses a device that no longer exists, so
concurrent refreshes rotate one after another. Both refresh endpoints classified
the presented token before taking that lock, so two refreshes of one token
could still both find it current and rotate it twice; they now rotate through
`rotate_refresh_token`, which classifies the token again under the lock, and
the later request takes the grace path and returns the same successor.
Upstream has the same bug. Files: `src/service/users/device.rs`,
`src/api/client/session/refresh.rs`, `src/api/oidc/token.rs`; tests in
`src/main/tests/refresh_removed_device.rs` and
`src/main/tests/refresh_concurrent.rs`.

### A refresh token does not authenticate requests

Refresh tokens are stored in the same token map as access tokens, so request
authentication accepted a refresh token as a bearer token. Used that way it
stayed valid until the device next rotated it, since refresh tokens carry no
expiry under the default `refresh_token_ttl = 0`, and it never went through the
rotation and reuse detection that `/refresh` applies. A presented token with
the `refresh_` prefix every refresh token carries now authenticates no client
API request and is refused like an unknown token; `/refresh` and the OIDC token
endpoint still redeem it, and OIDC revocation still accepts either kind.
Upstream has the same bug. File: `src/api/router/auth.rs`; test in
`src/main/tests/refresh_token_bearer.rs`.

### CORS allows the Cloudflare Access token header

MindRoom Chat on iOS reaches a homeserver behind Cloudflare Access by sending its
Access token in a `cf-access-token` header on every Matrix request, because its
web view cannot hold the Access cookie. Access passes CORS preflights through to
the origin, so the homeserver answers every preflight and must allow that header.
The homeserver ignores the header. File: `src/router/layers.rs`.

### OpenID tokens can be bound to a relying party audience

Files:
- `src/api/client/openid.rs`, `src/api/server/openid.rs`
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

Files: `src/api/server/openid.rs`, `src/api/router.rs`; test in
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
- `src/api/client/ephemeral.rs`, `src/api/router.rs`
- `src/api/client/sync/v3.rs`, `src/service/sync/watch.rs`
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
  400 `M_UNKNOWN`, a sender who is not joined 403 `M_FORBIDDEN`, a body over
  65536 bytes 413 `M_TOO_LARGE`, a body that is not an object 400
  `M_BAD_JSON`, and more than 30 events from one sender to one room in a
  one-second window 429 `M_LIMIT_EXCEEDED` with `retry_after_ms` until the
  window ends. A transaction (type, sender, device, txnId) that is still
  buffered answers `{}` without a second copy.
- Events live only in memory. Each room keeps at most 256 events and 1 MiB of
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

### 1) `mindroom/edits: compact /sync, purge superseded edits, bundle the survivor`
Files:
- `src/api/client/sync/mod.rs`, `src/api/client/sync/mindroom_edits.rs`
- `src/core/config/mod.rs`, `src/core/config/check.rs`
- `src/core/matrix/event.rs`, `src/core/matrix/event/relation.rs`
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
set and removal cookie paths) was merged upstream, so it is no longer a fork
delta.

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
- `src/service/pusher/mod.rs`, `src/service/pusher/send.rs`,
  `src/service/pusher/append.rs`
- `src/service/pusher/tests.rs`
- `src/mindroom-tests/tests/stream_reply_push.rs`

Behavior:
- Recognizes MindRoom streamed events by the `io.mindroom.stream_status`
  content key (an event-level protocol signal: senders opt their own streamed
  events in; no account privilege involved).
- Suppresses push notifications for stream updates in non-terminal states, so
  a streaming agent reply does not notify once per chunk. Unknown stream
  statuses fail closed (suppressed) so a new producer state cannot
  reintroduce push spam.
- When the stream reaches a terminal status (`completed`, `cancelled`,
  `interrupted`, `error`) via an `m.replace` edit, evaluates the surviving
  message as the final content it represents: for `m.room.message` the
  `m.new_content` body, for `m.room.encrypted` the payload without
  `m.relates_to`. Ordinary room/DM/mention/mute push rules then decide the
  single notification instead of the generic edit-suppression rule swallowing
  the terminal update.
- A terminal edit adds no unread count (notification or highlight) when the
  recipient has read the event it replaces: the read watermark for the count
  it would add to (its thread, or the room for a main-timeline reply) covers
  it, or the user's own send in that thread came after it (a follow-up, or
  the reaction that stops the stream; the unthreaded private read marker
  records it, since the own send's reset deletes thread watermarks). A
  `"main"` receipt or an own send elsewhere in the room covers no thread.
  Clients send no receipt for an edit of a reply they already read, so a
  user who watched a reply finish would otherwise keep a count, and an
  app-icon badge, with nothing left to read. The push itself is still
  delivered.
- The `push_everything` debug mode honors the same suppression.
- Keeps upstream's recipient-based membership-notification target flag. The
  real gateway fixture checks an invite addressed to someone other than its
  sender, alongside terminal-stream notification behavior.

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
- Pins CI rustfmt to `nightly-2026-10-04`, which passes upstream `v1.9.4`
  unchanged; upstream CI floats `nightly`. The locked Fenix formatter in
  `flake.lock` (nightly-2026-08-05) reports nine upstream files, so the
  development shell's `cargo fmt` must not be used to reformat. Keep the two CI
  formatter commands aligned.
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
concurrency, real gateway stream/invite notifications, and audience-bound
OpenID tokens with the option on and off (`openid_audience.rs`,
`openid_audience_disabled.rs`), and OpenID userinfo room membership
(`openid_room_membership.rs`). Stream classification
also has unit tests in `src/service/pusher/tests.rs`.

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
# bundle_edit_relations defaults to true in the fork; set false only to opt out.
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
- A client that lost its crypto store but kept its access token receives 403
  `M_FORBIDDEN` on `/keys/upload` until it logs in again (device identity keys
  are immutable per device id).
- Non-terminal `io.mindroom.stream_status` events do not push; terminal events
  use ordinary recipient push rules. The classifier does not deduplicate
  multiple distinct terminal events for the same stream.
- OpenID audience binding is off unless `mindroom_openid_audience = true`.
  With it on, tokens requested without `io.mindroom.audience` stay unbound and keep verifying at userinfo calls without the query audience, so Element Call, LiveKit, the hosted provisioning service, and stock relying parties work unchanged.
  A relying party that sends `io.mindroom.audience` to userinfo accepts only tokens bound to that audience, so its clients must request bound tokens.
  Conversely, a client that requests a bound token can only use it at relying parties that send the matching audience; relying parties that send none get 401.
  Enable the option only after MindRoom backends and MindRoom Chat clients, including the iOS app, send audiences.
- Upstream v1.9.1 `/messages` treats `from` and `to` as directional
  stream-position bounds and
  rejects malformed pagination tokens with `M_INVALID_PARAM`.
