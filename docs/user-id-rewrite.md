# Renaming a local user in place

Matrix user IDs are permanent. This fork adds `query user-id-rewrite`, an admin
command that rewrites one local user ID to another directly in the database, so
the account keeps its password, profile, rooms, memberships and history under
the new ID. It runs only on a stopped server, through `--execute`.

Use it only for users whose rooms are not shared with other servers. Rewritten
events no longer match their hashes and signatures, so a federated server that
already holds them would reject them.

## What it does

`scan` reads every database map and writes a JSON report: matches per map, the
planned action for each, and up to `--sample-limit` excerpts per map. It never
writes. `apply` makes the same plan, refuses if anything is unplanned, writes,
then scans again and prints how many matches remain.

Each map has a class:

| Class | Maps (examples) | Action |
|---|---|---|
| Raw replace | `userid_password`, `userid_displayname`, membership indexes, account data, receipts, `mediaid_user` | Replace the ID in keys and values, including other users' records that mention it (such as `m.direct`) |
| Clear | devices, access and refresh tokens, transaction IDs, device keys, one-time and fallback keys, pushers, filters, to-device queue | Delete matching records: signed key material would not verify under a new ID |
| Clear own keys only | `keyid_key`, key backups | Delete records keyed by the user; a match only in a value (another user's key signed by this user) blocks `apply` |
| Event graph | `pduid_pdu`, `eventid_originalpdu`, `eventid_outlierpdu`, `referencedevents`, `roomid_pduleaves` | Replace only with `--rewrite-event-graph` |
| Skip | `tokenids` (search index) | Never read: search words never contain `@` or `:` |
| Unknown | anything else | Any match blocks `apply` |

A match is a full user ID that does not continue a server name: a letter or
digit, a `.` or `-` before one, or a `:` before a port digit after it rejects
the match. Mentions followed by punctuation, and MatrixRTC membership IDs
(`<user_id>:<device_id>`), are rewritten. URL-encoded `matrix.to` links and the
bare localpart are counted in the report but not rewritten.

`apply` refuses without `--confirm-offline-clone`, on a read-only database, when
the new ID already exists, on any blocked record, and on a key collision.
Reports are written mode 0600 and carry no samples from token, device, key or
password maps. They do contain message excerpts; delete them afterwards.

## Procedure

1. Stop the server and everything that writes through it (appservices,
   bridges, bots using the account). Take a filesystem snapshot of the
   database directory. It is the only rollback.
2. Write a config for the offline run. Use the server's `server_name` and
   `database_path`, an unused `port`, `allow_federation = false`, and
   `redaction_retention_seconds = 0` (the retention worker otherwise races the
   rewrite). With MindRoom's edit purge enabled, also set
   `mindroom_edit_purge_enabled = false`.
3. Scan:

   ```bash
   tuwunel -c offline.toml \
     --execute "query user-id-rewrite scan --from @old:example.org --to @new:example.org --report /tmp/rename/scan.json" \
     --execute "server shutdown"
   ```

   Continue only if `blocked_records` and `to_occurrences` are 0 and the
   per-map counts look right.
4. Apply:

   ```bash
   tuwunel -c offline.toml \
     --execute "query user-id-rewrite apply --from @old:example.org --to @new:example.org --report /tmp/rename/apply.json --confirm-offline-clone --rewrite-event-graph" \
     --execute "server shutdown"
   ```

   It must print `0 matches remain`. If it fails or is interrupted, restore the
   snapshot. Never run `apply` again on a partly written database.
5. Start the server normally. The old ID can no longer log in; the new one logs
   in with the old password and gets a new device and new cross-signing keys.
6. Clients that synced before the rename keep the old ID in their member lists
   until they clear their cache. Re-sending the user's own member event,
   unchanged, in every joined room makes them see the new ID join:

   ```bash
   for room in $(curl -sf -H "Authorization: Bearer $TOKEN" "$HS/_matrix/client/v3/joined_rooms" | jq -r '.joined_rooms[]'); do
     path="$HS/_matrix/client/v3/rooms/$(jq -rn --arg r "$room" '$r|@uri')/state/m.room.member/$(jq -rn --arg u "$NEW" '$u|@uri')"
     curl -sf -H "Authorization: Bearer $TOKEN" "$path" | curl -sf -X PUT -H "Authorization: Bearer $TOKEN" -d @- "$path" > /dev/null
   done
   ```

7. Update applications that stored the old ID themselves (bots, bridges, MindRoom).

## Costs

- Other users see the renamed user's old encrypted messages as sent by an
  unknown device, and the renamed user cannot decrypt encrypted history from
  before the rename.
- Rewritten events fail signature checks if they ever reach another server.
- The search index keeps the words of rewritten messages as they were, so a
  search for the new localpart does not find mentions written before.

## Scale

On `mindroom.chat` (1.2 GB database, 4 Neoverse-N1 cores) the scan took 43 s
and the apply 68 s, rewriting 85,264 records and clearing 202,167. Peak memory
was under 1 GB.
