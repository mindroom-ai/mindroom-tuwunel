# MindRoom Tuwunel Fork - Changes vs Upstream

This document describes the current MindRoom fork behavior on top of upstream
`tuwunel`. The fork is rebased directly onto upstream commits; see
`docs/rebase-*.md` for the per-rebase log. As of the 2026-10-06 rebase the base
is upstream `main` at `3f5db6d3a`, 134 commits past the `v1.9.3` release (the
workspace version is still 1.9.3). Earlier changes merged upstream and dropped from the
fork include the room-creation default power-level
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
backported from `main` and that `v1.9.3` does not contain are carried as
individual cherry-picks (see below). The fork's sliding sync profiles fix
(#60) was dropped at that rebase because its test passes on v1.9.3 without
it, but upstream's rewrite of the profiles extension (`005830a1b`) only checks
that each user shares a joined room with the requester, so the fix is carried
again (see "Sliding sync profile changes only for joined rooms").
See the [v1.9.3 rebase record](docs/rebase-v1.9.3-2026-10-05.md).

On upstream `main` (`3f5db6d3a`), the fork's thread summary fixes (#19-#21)
are upstream: PRs #617-#619 (`c9307347a`, `787099ab5`, `e6735fdf3`) and their
rework into a single `rebuild_thread_summaries` startup pass (`b0c52a081`),
which replaces the fork's `recount_thread_replies` and
`scrub_redacted_thread_latest` passes and runs once on a fork database. Only
the bound on the search for a redacted latest reply's replacement (#24) stays
in the fork, on upstream's rewritten helpers. All eight temporary backports are
in `main` and are no longer carried. Account deactivation now holds upstream's
admin-room lock and refuses the last admin (`3fe9ef681`, `5420f2e56`); the fork
records its deactivation reason under that lock.
See the [upstream main rebase record](docs/rebase-upstream-main-2026-10-06.md).

## How To Inspect
- Fork commits: `git log --reverse --oneline <upstream-base>..HEAD`
- Files changed in the fork: `git diff --stat <upstream-base>..HEAD`
- Per-commit patch: `git show <sha>`

Keep the delta limited to fork behavior and required compatibility changes.
Leave unrelated upstream tests and tooling unchanged; handle host-specific
verification needs in the test environment instead of carrying extra patches.

The history keeps the v1.9.1 ownership layout of eight commits (shared test
infrastructure, five runtime features with their tests and compatibility
changes, CI, and docs), followed by the squash-merged fork PRs #15, #17, #18
and #22-#24 in their original order, the fork PRs #26-#59 and #61-#67 in merge
order, the v1.9.3 rebase record, and this rebase's documentation. No upstream
backports are carried.
See the [current ownership and rebase procedure](docs/rebase-upstream-main-2026-10-06.md).
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
answers are a few kilobytes, but a slow answer kept up to 256 MiB buffered per
request until the read timed out, and a large one was decoded in full before
anything checked it. A room summary also asked every server in its `via` list,
even one named many times, 32 at a time. The room summary now asks each of the
first 8 distinct servers once and reads at most 1 MiB from each, alias answers
and signing-key documents (from the origin or from a notary for one server)
are read up to 1 MiB, and profiles up to four times the 64 KiB profile limit.
A larger answer is dropped while it is read, and no limit exceeds
`max_response_size`. Upstream has the same bug. Files:
`src/service/federation/{execute.rs,feds.rs}`,
`src/api/client/room/summary.rs`, `src/service/rooms/alias/mod.rs`,
`src/service/server_keys/request.rs`, `src/service/profile/{mod.rs,remote.rs}`;
test in `src/main/tests/federation_lookup_response_limits.rs`.

### Remote profile lookups replace the cached profile within the local limits

Looking up a remote user's profile fetches it from their server on every
request. Each returned field was stored without the field-name grammar and
64 KiB total size that local profiles are held to, and a field the server no
longer returned was never removed, so the cached profile kept stale fields and
grew with every new field name a server returned. The response now replaces the
cached profile, as `!admin users refresh-profile` already did, and a response
with a field name outside the MSC4133 grammar, over 64 KiB, or with more than
100 fields is refused without being stored. The field cap bounds the work of
one lookup, which rewrites and logs every field the server serves or drops.
Upstream has the same bug. Files:
`src/service/profile/{mod.rs,remote.rs}`; test in
`src/service/profile/tests/remote/mod.rs`.

### Replacing a redacted latest reply searches a bounded range

Redacting a thread's newest reply swaps the newest remaining reply into the
root's `m.thread` summary, or drops the summary when none remains. Upstream
walks every relation of the root to find it, loading each event, while the
redaction holds the room lock and the sequence permit; reactions, edits and
redacted replies stay in the relation index. Only the root's newest 256
relations are searched, counting ones whose event no longer loads (such as
purged edits), and a root with no remaining reply among them drops the summary
too. The startup summary rebuild still reads every relation. Upstream has the
same bug. Files: `src/service/rooms/{threads/mod.rs,pdu_metadata/relations.rs}`;
test in `src/service/rooms/threads/tests/redact.rs`.

### Redacting a message redacts its edits

A redaction pruned only its target, so the target's `m.replace` edits kept the
new text in `m.new_content` and stayed in the search index. Any member could
still read a deleted message's latest text by the edit's id, from the target's
relations, in `/messages` or through `/search`. A redaction now also redacts
the target's edits that the edit bundle counts (same sender and type), with the
same redaction as their reason. Upstream redacts only the target too. Files:
`src/service/rooms/{timeline/append.rs,pdu_metadata/bundling.rs}`; test in
`src/main/tests/redaction_edits.rs`.

### Sliding sync profile changes only for joined rooms

The MSC4262 profiles extension of simplified sliding sync read the profile
change log of every room in the window and every room the connection had seen,
without checking the user's membership. A room the user has left or been
removed from, which a room subscription keeps in the window, and rooms they are
invited to or have knocked on kept sending their members' profile changes
whenever the member also shared another room with the user. Rooms in the
window now follow the required-state rule, as receipts and typing do, and a
room known only to the connection counts while the user is joined. Upstream
has the same bug. File: `src/api/client/sync/v5/extensions/profiles.rs`; test
in `src/main/tests/sync_v5_profiles.rs`.

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

### EDUs sent to other servers are bounded in size

Device keys and cross-signing keys a local client uploaded were stored with no
size limit and sent to other servers unchanged in `m.device_list_update` and
`m.signing_key_update` EDUs, and a to-device message for a remote user went out
as an EDU as large as the client made it. A peer refuses a transaction over its
request body limit, and the refused transaction's rows are sent again with
every later transaction to that peer, so one oversized EDU stopped outbound
federation to it. Device keys, including those of a dehydrated device, and
each cross-signing key are now refused with 413 `M_TOO_LARGE` over 64 KiB, the
limit Synapse applies to to-device messages, and so is a to-device message for
a remote user whose EDU, with its event type and device ID, would be larger
than that. A device ID a client chooses, which device list updates carry
twice when device names are not federated, is refused with 400
`M_INVALID_PARAM` over 512 bytes, the limit Synapse applies at login. A
federation transaction also keeps each EDU, in order, that still fits within
8 MiB together with those already kept, leaves out the others with a warning
and acknowledges their rows with the transaction, so oversized EDUs, including
ones queued earlier, no longer hold up the destination. Upstream has the same
bug.
Files:
`src/api/client/{to_device.rs,dehydrated_device.rs,keys/upload_keys.rs,keys/upload_signing_keys.rs}`,
`src/service/users/device.rs`,
`src/service/sending/sender/{mod.rs,dispatch/federation.rs}`; tests in
`src/main/tests/edu_content_size.rs`, `src/service/sending/sender/tests.rs` and
`src/service/sending/sender/dispatch/federation/tests.rs`.

### Federated read receipts need a joined user and an event of the room

An `m.receipt` EDU from another server was stored for any of that server's
users once it had a member in the room, whether or not the named user was
joined, and for any event id and `thread_id` it named. Users outside the room
then showed up as readers, and each new user or `thread_id` string stored
another receipt row, kept until the room is deleted. A federated receipt is now
stored only for a user joined to the room, as typing notifications already
require, at an event of the room's timeline, and with no `thread_id`, `main`, or
a thread root that is an event of the room. Upstream has the same bug. Files:
`src/api/server/send.rs`, `src/api/client/read_marker/mod.rs`; test in
`src/main/tests/federation_receipt_edu.rs`.

### Banned rooms refuse member events sent through `/state`

`/join`, `/knock` and `/invite` refuse a room the server admin banned, but the
same member events sent through `PUT /rooms/{id}/state/m.room.member/{user}`
were accepted. A non-admin's member event in a banned room is now refused with
`M_FORBIDDEN` unless it is a leave or a ban, as in Synapse; this includes a
per-room profile update. Upstream has the same bug. Files:
`src/api/client/state.rs`; test in `src/main/tests/state_member_banned_room.rs`.

### `ban-room` makes local members leave the room

`ban-room` bans the room before making its local members leave, and leaving a
banned or disabled room only cleared the membership cache. The member events in
room state stayed `join`, so remote servers still listed the evicted users, and
their join still authorized the events they sent afterwards. Leaving such a
room now sends the leave event when the server is in the room; only leaving
through another server is still skipped. Upstream has the same bug. File:
`src/service/membership/leave.rs`; test in
`src/main/tests/ban_room_member_leave.rs`.

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

### Federated key claims keep only the answering server's users

A remote server's `/user/keys/claim` answer was taken as a whole, so entries
naming users of other servers, local users included, replaced those users'
one-time keys in the client's `/keys/claim` response; a local user's key had
already been taken from storage and was lost. Entries whose user does not
belong to the answering server are now dropped, as federated key queries
already do. Upstream has the same bug. File:
`src/api/client/keys/claim_keys.rs`.

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

### Appservice account data stays within its user namespace

The account-data endpoints (`GET`, `PUT` and MSC3391 `DELETE`, global and per
room) let any appservice request act on the path `userId`, so an appservice
could read, change or delete the account data of users outside its
registration. An appservice may now act only on its own sender and the users in
its `users` namespace, as the profile endpoints already require. Upstream has
the same bug. Files: `src/api/client/account_data/mod.rs`; test in
`src/api/client/account_data/tests.rs`.

### send_join response events are checked before they are stored

Joining a room over federation stored every event of the `send_join` response
as an outlier, replacing any copy this server already had. An event whose
content no longer matched its content hash was stored as received instead of
redacted, its `unsigned` data was kept, and `auth_chain` events were not checked
to belong to the joined room. Such events are now redacted, `unsigned` is
dropped, and `auth_chain` events go through the same room and format checks as
`state` events. An event in this server's timeline keeps its stored copy, and
so does any stored event whose new copy had to be redacted; a copy that matches
its hash still replaces an outlier, such as the unchecked knock state stored
before the join. In room version 12, where `m.room.create` has no `room_id`,
the joined room's ID was also written into any create event of the response,
replacing the `room_id` a v11 create carries, so another room's create could
replace that room's stored copy. A create event now keeps its own `room_id`,
and a v12 create is accepted only when the room ID is derived from its event
ID. Upstream has the same bug. Files: `src/service/server_keys/verify.rs`,
`src/service/membership/join.rs`, `src/core/matrix/pdu.rs`,
`src/core/matrix/pdu/format.rs`; test in `src/service/membership/join/tests.rs`.

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
local user other than the joining user now stays, unless the answer has one
naming it as an auth event, such as a new invite. Upstream has the same bug.
Files: `src/service/membership/join.rs`; test in
`src/service/membership/join/tests.rs`.

### Failed appservice requests leave the `hs_token` out of the log

An appservice request also sends the `hs_token` as the legacy `access_token`
query parameter, and when it could not be sent (connection refused, timeout)
the logged `reqwest` error printed the full request URL with that token. The
error now drops its URL before it is logged or returned, as federation requests
already do; the log line still names the appservice and its registered URL.
Upstream has the same bug. Files: `src/service/appservice/{request.rs,ping.rs}`;
test in `src/main/tests/appservice_request_error.rs`.

### Federation requests follow no redirects

The federation clients followed a peer's redirects, and a redirect target was
never checked against `ip_range_denylist`, so a peer could send a request on to
any address the server can reach, including over plain HTTP. A legacy media
fetch then stored that address's answer as the peer's media and returned it to
the requesting client. Federation requests now follow no redirects, and legacy
media requests to peers no longer set `allow_redirect`, so the peer serves the
media itself. Upstream has the same bug. Files: `src/service/client/mod.rs`,
`src/service/media/remote.rs`, `src/api/client/media_legacy.rs`; test in
`src/main/tests/federation_redirect.rs`.

### Server discovery follows redirects only to HTTPS

The `/.well-known/matrix/server` lookup followed up to four redirects without
checking them, including from HTTPS to plain HTTP, so a peer's well-known
document could send the lookup on to any address the server can reach. Each
hop must now be an HTTPS URL that passes the redirect check the media, URL
preview and push clients use, which refuses IP literals in
`ip_range_denylist`. Redirects are still followed, as the spec asks. Upstream
has the same bug. File: `src/service/client/mod.rs`; test in
`src/main/tests/well_known_redirect.rs`.

### Per-user room lists stop at the user ID

`rooms_joined`, `rooms_invited`, `rooms_knocked` and `rooms_left` scanned their
`(user_id, room_id)` indexes with the bare user ID as the prefix, so the rooms
of a user whose ID extends it (`@alice:example.org.other` for
`@alice:example.org`) were listed as the shorter user's own, for example in
`/sync` and `/joined_rooms`. The scans now include the key separator after the
user ID, as the per-user state scans already did. Upstream has the same bug.
Files: `src/service/rooms/state_cache/mod.rs`; test in
`src/service/rooms/state_cache/tests.rs`.

### Public read receipts need a joined user and an event of the room

`POST /rooms/{roomId}/receipt/m.read/{eventId}` and the `m.read` field of
`/read_markers` stored a public read receipt without checking that the sender
is joined to the room or that the event belongs to it. A user outside the room
then showed up as a reader to its members and to other servers, and a receipt
whose `thread_id` named the same unknown event passed the MSC3771 thread check,
so each new event id stored another receipt row, kept until the room is
deleted. Both endpoints now answer 403 to a user who is not joined and 404 for
an event that is not in the room's timeline, as private read markers already
do. Upstream has the same bug. Files:
`src/api/client/read_marker/{mod.rs,receipt.rs,read_markers.rs}`; test in
`src/main/tests/public_receipt_room.rs`.

### Prev events from another room are rejected

The prev-event walk checks that each event it visits is in the incoming PDU's
room, but prev events already in the timeline skipped the walk and that check.
The walk also stopped at a prev event exactly as old as the room's first event,
which was still added to the timeline, so its own prev events went unchecked.
A PDU could then name another room's event in `prev_events`, directly or through
such a prev event, and the state at that event became the state before it: the
event was authorized against and stored with the other room's state, and for a
state event that state was also resolved into the room's current state. Prev
events already in the timeline now get the same room check, and the walk
continues past events as old as the room's first event, so such a PDU is
rejected. Upstream has the same bug.
Files: `src/service/rooms/event_handler/fetch_prev.rs`; test in
`src/main/tests/federation_prev_event_room.rs`.

### The state before an incoming event comes only from its room

The state before an incoming PDU is derived from the state at its prev events,
built locally from the events we hold, or taken from the sending server's
`/state_ids` answer. Each looks events up by id alone. A prev event that was
missing during the prev-event walk, and so skipped its room check, could be in
another room's timeline by the time its state was used; the local build
followed a held outlier's prev events into another room; and a `/state_ids`
answer could name events we hold from another room. The PDU was then
authorized against and stored with that other room's state. Each of these now
checks the room of every event it loads: the derived state and `/state_ids`
reject such a PDU, and the local build falls back to `/state_ids`. Upstream
has the same bug. Files:
`src/service/rooms/event_handler/{fetch_state,state_at_incoming,state_local_build}.rs`;
test in `src/main/tests/federation_prev_event_room.rs`.

### A room keeps its one create event

In room versions 1 to 11, an `m.room.create` event for a room this server is
in, other than the room's own create, passed the authorization rules when it
came from the server named in the room ID, since a create event has no rules
that depend on the room state. It was added to the timeline and resolved into
the current state, where it could take the place of the room's create. Such an
event is now refused before it reaches the timeline or the room state, whether
it arrives in a transaction or as a fetched prev event. The authorization rules
are unchanged. Upstream has the same bug.
File: `src/service/rooms/event_handler/upgrade_outlier_pdu.rs`; test in
`src/main/tests/federation_second_create.rs`.

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

### Email password resets leave deactivated accounts deactivated

Deactivation without erasure keeps the account's email binding, and a
logged-out password reset through that email stored the new password without
checking the account, which made a deactivated account usable again. The reset
now refuses a deactivated account with `M_USER_DEACTIVATED`, as login does.
Upstream has the same bug. Files: `src/api/client/account/change_password.rs`;
test in `src/main/tests/email_password_reset/scenarios.rs`.

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
events against `max_fetch_prev_events`. The queued fetches all run at once and
keep their results, so one incoming event could start about 20 times that many.
The walk now counts every event it queues. The `/get_missing_events` batch
fetched before the walk asks for 10 events but stored every event the remote
server answered with; it now stores at most 10. Each auth chain fetch started
for the incoming event, for its own auth events, for each prev event the walk
fetches and for each event of that batch, also had its own limit of
`max_fetch_prev_events` times 64 KiB; they now share one. Upstream has the same
bugs. Files: `src/service/rooms/event_handler/fetch_prev.rs`,
`src/service/rooms/event_handler/fetch_auth.rs`,
`src/service/rooms/event_handler/handle_incoming_pdu.rs`; tests in
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

### `ip_range_denylist` covers IPv4-mapped IPv6 addresses

An IPv4-mapped IPv6 address (`::ffff:a.b.c.d`) was matched against the denylist
as an IPv6 address, so the IPv4 ranges never matched it, while a connection to
it reaches the embedded IPv4 host. The URL, redirect, DNS answer and peer
address checks now match such an address as its IPv4 address, so
`[::ffff:10.0.0.1]` is refused like `10.0.0.1`. Upstream has the same bug.
File: `src/service/client/mod.rs`; test in `src/service/client/tests.rs`.

### Default `ip_range_denylist` covers the unspecified addresses

On Linux a connection to `0.0.0.0` or `::` reaches the local host, but the
default denylist covered the local host only as `127.0.0.0/8` and `::1/128`, so
a pusher or media URL using the unspecified address was not refused like one
using `127.0.0.1`. The default now also lists `0.0.0.0/8` and `::/128`; Synapse
always refuses `0.0.0.0` and `::`. Upstream has the same default. Files:
`src/core/config/mod.rs`, `tuwunel-example.toml`; test in
`src/core/config/tests.rs`.

### Federation parses bracketed IPv6 literals for `ip_range_denylist`

The federation checks of an IP literal destination parsed the host with its
brackets (`[::1]`), which fails for every IPv6 address. A server name that is
an IPv6 literal was therefore always refused, and a request to an IPv6 literal
that a server name delegates to was sent without the denylist check. Both
checks now parse the address without brackets and use the same address check
as the other clients, which also matches IPv4-mapped addresses against the
IPv4 ranges. Upstream has the same bug. Files:
`src/service/resolver/actual.rs`, `src/service/federation/execute.rs`,
`src/service/client/mod.rs`; test in `src/service/resolver/tests.rs`.

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

### Sliding sync withholds the timeline of knocked rooms

Sliding sync lists include rooms the user has knocked on, but only invited rooms
had their timeline withheld, so a knocker received the room's newest events as
a member would. The timeline now follows the same membership check as the
required state, which already excluded invitees and knockers. Upstream has the
same bug. Files: `src/api/client/sync/v5/rooms.rs`; test in
`src/main/tests/sync_v5_knock_timeline.rs`.

### Knock state leaves local memberships alone

A knock on a room this server is not in installs the answering server's
`knock_room_state` as the room's state, unchecked, and forcing that state
replayed each `m.room.member` event in it into the membership cache, so it
could mark other local users as joined, invited, or no longer invited. Member
events are now left out of knock state; the knocking user's own membership
still comes from the knock event this server builds, and other local users
keep the member events the room's state has for them. Upstream has the same
bug. Files: `src/service/membership/knock.rs`; test in
`src/service/membership/knock/tests.rs`.

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

### A pending knock does not open a room over federation

The federation room access check (`/state`, `/state_ids`, `/event`,
`/event_auth`, `/backfill`, `/get_missing_events`, `/timestamp_to_event`)
admitted every server while any user, local or remote, had a pending knock in
the room. A server with no joined member could then read the room's state and,
with shared history visibility, its timeline. A pending knock no longer counts:
the requesting server needs a joined member unless the room is world-readable.
Upstream has the same bug. Files: `src/api/server/{utils.rs,event.rs}`; test in
`src/main/tests/federation_knock_access.rs`.

### Leaving without a membership records no departure

When room state held no member event for the user, or a `leave` or `ban` one,
`/leave` still wrote a leave row at a fresh stream position. A user who had
never joined then got the room in `/sync` as a left room, with its recent
timeline and current state, and a kicked or banned user's departure moved
forward past the events their removal hides. The leave row is now written only
when it clears a cached join, invite or knock. Upstream has the same bug.
Files: `src/service/membership/leave.rs`; test in
`src/main/tests/leave_without_membership.rs`.

### Federated key queries keep only the answering server's users

A remote server's `/user/keys/query` answer was taken as a whole, so entries
naming users of other servers, local users included, had their master key
stored as that user's and replaced the local keys in the client's response.
Entries whose user does not belong to the answering server are now dropped, as
the device list and signing key update EDUs already do. Upstream has the same
bug. File: `src/api/client/keys/get_keys.rs`.

### Remote hierarchy answers leave summaries of local rooms alone

A remote server's `/hierarchy` answer for a space was cached for every child
it listed, so the cached summary (name, topic, avatar, join rule, member count)
of a room this server is in could be replaced by the remote's version, which
`/hierarchy` then served until the room's state changed or the entry expired.
Children this server is in are now skipped when caching such an answer; their
summaries come from local state. Upstream has the same bug. File:
`src/service/rooms/spaces/federation.rs`; test in
`src/service/rooms/spaces/tests.rs`.

### Remote hierarchy answers must describe the requested room

A remote server's `/hierarchy` answer for a room this server is not in was
cached and served as that room's summary without checking the summary's
`room_id`. An answer describing a different room, possibly one this server is
in, then appeared in `/hierarchy` as that other room, with the name, topic,
avatar and join rule the remote gave it, until the cache entry expired. Such
answers are now ignored, as `/summary` already ignores them. Upstream has the
same bug. File: `src/service/rooms/spaces/federation.rs`; test in
`src/main/tests/federation_hierarchy_room.rs`.

### Sliding Sync caps the timeline limit

Sliding Sync lists and room subscriptions passed their `timeline_limit` to the
timeline loader without a ceiling, so a large value read a room's whole history
into one response. The limit is now capped at 100 events, as legacy `/sync`
caps a filter's timeline limit; a room with more new events comes back
`limited` with a `prev_batch`. Upstream has the same bug. File:
`src/api/client/sync/v5/rooms.rs`.

### Sync caps the requested profile fields

The MSC4262 `profiles.fields` list of a sliding sync request and the MSC4429
`profile_fields.ids` of a legacy `/sync` filter had no length limit. Each
requested field costs one read for every user the response carries: on an
initial pass, every member of a room unless members are lazy-loaded. Sliding
sync also compared each request's list with the connection's previous one name
by name. Both now use only the first 64 names; sliding sync keeps only those on
the connection, and legacy sync cuts stored and inline filters alike. Upstream
has the same bug. Files: `src/service/{profile/mod.rs,sync/mod.rs}`,
`src/api/client/sync/profiles.rs`; tests in `src/service/sync/tests.rs` and
`src/main/tests/sync_v3_profiles.rs`.

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
move this point: these routes and `/initialSync` serve the state after the
leave or ban that ended the user's last join, found by the same walk that
bounds their history. Upstream has the same bug. Files:
`src/service/rooms/state_accessor/user_can.rs`,
`src/api/client/{state.rs,membership/members.rs,room/initial_sync.rs}`; tests
in `src/main/tests/{state_departed_member.rs,knock_withdrawal_history.rs}`.

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

### UIAA keeps only small request bodies for pending sessions

A UIAA request sent without `auth` keeps its JSON body in memory so the
follow-up request can omit fields, and nothing removed a body again, not even
when its session finished. The bodies now live in an LRU of 1024 sessions,
bodies over 4 KiB of serialized JSON are not kept (the client resends the full
request, as after a restart), and a finished session releases its body.
Upstream has the same bug. Files: `src/service/uiaa/mod.rs`; test in
`src/service/uiaa/tests.rs`.

### `/context` keeps current state from requesters who may not read it

A room's create event and backfilled events have no state snapshot, so
`/context` around one of them returned the room's current state, even to a user
who had never joined. That fallback now applies only to a requester who passes
the `/state` check (`user_can_see_state_events`) or to the admin room-context
endpoint; anyone else gets an empty `state`. A former member gets the state
from when they left, as `/state` serves them. Upstream has the same bug. Files:
`src/api/client/context.rs`; test in
`src/main/tests/context_snapshotless_state.rs`.

### SSO provider chaining does not read `loginToken` from the redirect URL

The legacy `GET /_matrix/client/v3/login/sso/redirect/{idpId}` endpoint read a
`loginToken` query parameter and linked the identity that signed in next to
that token's account, ahead of the account the identity was already linked to.
It served the multi-provider chain, whose callback sent the browser back
through the endpoint with a fresh token, but nothing tied the token to the
browser presenting it. The callback now starts the next provider's sign-in
itself with the account it just signed in, and the endpoint ignores
`loginToken`. Upstream has the same bug. File:
`src/api/client/session/sso.rs`; test in `src/main/tests/sso_login_redirect.rs`.

### Legacy SSO login asks before an unlisted `redirectUrl`

The legacy SSO callback sent the fresh login token to whatever `redirectUrl`
the sign-in link named. A target that is not on the `well_known.client`
origin, not waived as an OIDC client's redirect would be
(`oidc_require_client_approval`, `oidc_registration_allowed_redirect_hosts`)
and not listed in the legacy-SSO-only `sso_trusted_redirect_hosts` (web client
hosts and native app schemes) now gets the token only from a Continue link on
a page naming it, and such a `javascript:` target or one with userinfo is
refused. Upstream has the same bug. Files: `src/api/client/session/sso.rs`,
`src/api/oidc/complete.rs`, `src/api/router.rs`, `src/core/config/mod.rs`;
test in `src/main/tests/sso_login_redirect.rs`.

### Backfilled events do not set the old-event cutoff

A live federated event dated before the room's first stored event is skipped
as old, and the same cutoff bounds the fetch of its missing previous events.
Backfilled events sort before the rest of the timeline, so after a backfill
that first event carried a timestamp set by a remote server, and one dated in
the future made the server silently skip new events in the room until that
time passed. The cutoff now comes from the first event that was not
backfilled, which is the first event this server stored itself (the create,
our join or our knock), the same cutoff it used before any backfill. Upstream
has the same bug. Files:
`src/service/rooms/{timeline/mod.rs,event_handler/handle_incoming_pdu.rs}`;
test in `src/main/tests/incoming_after_future_backfill.rs`.

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

### SSO username fallback skips accounts linked to another identity

When every username a new identity claims at an untrusted provider is taken,
it falls back to a localpart derived from its issuer and subject. An existing
`sso`-origin account at that localpart was handed to it even when another
identity already signed in to that account, linking both identities to it. The
fallback now skips an account that has a linked identity, so the sign-in fails
with `M_USER_IN_USE`. An account with no linked identity, such as one whose
links an admin removed, is still reused at its fallback. Upstream has the same
bug. File: `src/api/client/session/sso.rs`; test in
`src/main/tests/sso_fallback_account.rs`.

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
- `src/service/pusher/mod.rs`, `src/service/pusher/send.rs`
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
- Pins CI rustfmt to `nightly-2026-10-04`, which passes upstream main
  `3f5db6d3a` unchanged; nightly-2026-09-26 reports one upstream file, and
  upstream CI floats `nightly`. The locked Fenix formatter in `flake.lock`
  (nightly-2026-08-05) reports nine upstream files, so the development shell's
  `cargo fmt` must not be used to reformat. Keep the two CI formatter commands
  aligned.
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
concurrency, and real gateway stream/invite notifications. Stream classification
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
Upstream's `auto_accept_invites.rs` reads the accepted room's `m.direct` once
after the join, racing the write that follows it; the fork polls for that write
first.

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
- Upstream v1.9.1 `/messages` treats `from` and `to` as directional
  stream-position bounds and
  rejects malformed pagination tokens with `M_INVALID_PARAM`.
