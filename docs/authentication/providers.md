# Identity Providers

Tuwunel can delegate login to external OAuth/OIDC identity providers. Each
configured provider appears as an option on the client's login page. Users are
redirected to the provider to authenticate, then returned to Tuwunel which
maps their identity to a Matrix account.

The same providers serve two login paths. Legacy clients use them directly
through the `m.login.sso` flow. Next-generation clients reach them indirectly:
Tuwunel's built-in [OIDC Authorization Server](oidc-server.md) is what those
clients authenticate against, and it in turn redirects the user to a provider
configured here to perform the actual login. A working next-gen setup therefore
needs at least one provider on this page plus the issuer URL described on the
OIDC server page.

### Provider guides

- [Authelia](providers/authelia.md)
- [Authentik](providers/authentik.md)
- [Keycloak](providers/keycloak.md)
- [Matrix Authentication Service (MAS)](providers/mas.md)
- _Please contribute documentation for yours here!_

## Configuring Tuwunel

Each provider is a `[[global.identity_provider]]` table in your configuration
file. Multiple providers can be configured by repeating the table header. Container users
please refer to the section on [environment variables](#configuring-via-environment-variables) instead.

### Required fields

| Field | Description |
|---|---|
| `brand` | Brand name of the provider: `Apple`, `Facebook`, `GitHub`, `GitLab`, `Google`, `Keycloak`, `MAS`, `Twitter`, or any custom string. Case-insensitive. Known brands get built-in defaults and workarounds. |
| `client_id` | The OAuth application ID issued by the provider. This becomes the provider's unique ID within Tuwunel and **must never change** — Tuwunel associates stored identities to it. |

### Authentication

| Field | Default | Description |
|---|---|---|
| `client_secret` | none | OAuth client secret issued by the provider. |
| `client_secret_file` | none | Path to a file containing the client secret. Ignored when `client_secret` is also set. Example: `/etc/tuwunel/.github_secret` |

### Discovery

| Field | Default | Description |
|---|---|---|
| `issuer_url` | brand default | Provider's OIDC issuer URL. Pre-supplied for well-known public providers. Required for self-hosted providers. Must match exactly what the provider expects and **must never change**. |
| `base_path` | brand default | Extra path after `issuer_url` leading to the `.well-known` directory. GitHub uses `login/oauth/`, for example. Pre-populated for known brands. |
| `discovery_url` | none | Fully overrides the `.well-known/openid-configuration` location. For developers or non-standard providers. |
| `discovery` | `true` | Whether to perform OIDC discovery at all. |

### Callback

| Field | Description |
|---|---|
| `callback_url` | The callback URL registered with the provider when you created the OAuth application. Must be exactly: `https://<your-matrix-server>/_matrix/client/unstable/login/sso/callback/<client_id>` |

### Login behavior

| Field | Default | Description |
|---|---|---|
| `default` | `false` | Mark this provider as the default for `/_matrix/client/v3/login/sso/redirect` (the endpoint without a provider ID). Required when multiple providers are configured and some clients (e.g. FluffyChat) need a single redirect target. If exactly one provider is configured it is implicitly the default. **(Experimental)** Multiple providers can share `default = true` — all must authorize successfully in sequence. |
| `name` | `brand` | Display name shown on the login page. Useful when multiple providers share the same brand. |
| `icon` | brand default | MXC URI for the provider's icon. Known brands have built-in icons. |
| `scope` | `openid email profile`; MAS: `openid` | List of OAuth scopes to request. An empty array uses the default shown here. Users can further restrict scopes during authorization. |
| `forward_action_prompt` | `false` | Forward the `action` query parameter from the SSO redirect endpoints to this provider as an OpenID Connect `prompt`. When enabled, `action=register` makes the upstream authorization request carry `prompt=create` so the provider shows its registration screen; `action=login` is left unforwarded. Only enable it for providers that support the OIDC `prompt=create` ("Initiating User Registration") extension. See [Registration hints](#registration-hints-for-oauth-aware-clients). |

### User ID mapping

| Field | Default | Description |
|---|---|---|
| `userid_claims` | all | Claims used to compute the Matrix localpart for new registrations. When empty, Tuwunel avoids generated IDs where possible. The special value `"unique"` forces generated IDs exclusively. The claim `"sub"` takes precedence over all others when listed. |
| `trusted` | `false` | Inverts user matching: instead of registering a new account when claims conflict with existing users, Tuwunel finds the first matching user and grants access to it. **Only set this for providers you self-host and fully control. Never use with public providers (GitHub, GitLab, Google, etc.); it enables account takeover.** For migrated databases, prefer durable bulk adoption or individual association to temporarily enabling this option. A trusted provider's `email` claim is also bound to a new account when it sends no `email_verified` claim. |
| `assigned_accounts` | `{}` | Assigns existing local accounts to people by the address in their `email` claim, matched ignoring ASCII case. A listed address's first sign-in gets that account or is refused, never another account. **Only for an app restricted to your own organization; whoever controls a listed address gets the account.** See [Assigning accounts created before the first sign-in](#assigning-accounts-created-before-the-first-sign-in). |
| `registration_rules` | `[]` | Rules a sign-in must meet to create an account, each a set of claims, the rooms a new account meeting it joins, and whether that account is external (joins only by invite). Empty, registration works as before. See [Letting only some people register](#letting-only-some-people-register). |
| `unique_id_fallbacks` | `true` | When no claim maps cleanly to an available username, generate a unique random localpart as a fallback. Set to `false` on private servers where random usernames are undesirable — a misconfiguration will produce an error instead. |
| `registration` | `true` | Whether this provider can create new Matrix accounts. Set to `false` to restrict the provider to existing users only. |

A new account takes its display name from the `name` claim and its avatar from
`avatar_url` or `picture`. The `email` claim becomes the account's email address
when the provider sends a true `email_verified` claim (a boolean, or the string
`"true"`), or when the provider is `trusted` and sends no `email_verified` claim.
Any other `email_verified` value counts as unverified. An address another
account already holds stays with that account.

### URL overrides

These override endpoints that are normally discovered automatically. Only use
them for non-standard or undiscoverable providers.

| Field | Description |
|---|---|
| `authorization_url` | Override the authorization endpoint. |
| `token_url` | Override the token endpoint. |
| `revocation_url` | Override the token revocation endpoint. |
| `introspection_url` | Override the token introspection endpoint. |
| `userinfo_url` | Override the userinfo endpoint. |

### Session

| Field | Default | Description |
|---|---|---|
| `grant_session_duration` | `300` | Seconds the authorization session stays valid before expiring (default: 5 minutes). |
| `check_cookie` | `true` | Verify the redirect cookie during the callback for CSRF protection. Disable only if a reverse proxy strips cookies. |


## Configuring via environment variables

For container deployments (Docker Compose, Podman, Kubernetes) where mounting a
configuration file is inconvenient, every provider field can be set via
environment variables instead.

The variable name is built from three parts joined by `__` (double underscore):

```
TUWUNEL_IDENTITY_PROVIDER__<index>__<FIELD>
```

- **`TUWUNEL_IDENTITY_PROVIDER`** — fixed prefix that maps to the
  `[[global.identity_provider]]` table array.
- **`<index>`** — an arbitrary string (typically `0`, `1`, `2`, …) that groups
  variables belonging to the same provider. All variables sharing the same
  index are treated as a single `[[global.identity_provider]]` entry. Indexes
  are sorted lexicographically, so numeric indexes give a predictable order.
- **`<FIELD>`** — the field name from the tables below, uppercased.

Multiple providers are expressed by using different indexes:

```env
# First provider — GitHub
TUWUNEL_IDENTITY_PROVIDER__0__BRAND="github"
TUWUNEL_IDENTITY_PROVIDER__0__CLIENT_ID="Ov23liYourGitHubClientId"
TUWUNEL_IDENTITY_PROVIDER__0__CLIENT_SECRET="your_github_secret"
TUWUNEL_IDENTITY_PROVIDER__0__CALLBACK_URL="https://matrix.example.com/_matrix/client/unstable/login/sso/callback/Ov23liYourGitHubClientId"

# Second provider — Google (marked as default)
TUWUNEL_IDENTITY_PROVIDER__1__BRAND="google"
TUWUNEL_IDENTITY_PROVIDER__1__CLIENT_ID="123456789-abc.apps.googleusercontent.com"
TUWUNEL_IDENTITY_PROVIDER__1__CLIENT_SECRET="GOCSPX-your_secret"
TUWUNEL_IDENTITY_PROVIDER__1__CALLBACK_URL="https://matrix.example.com/_matrix/client/unstable/login/sso/callback/123456789-abc.apps.googleusercontent.com"
TUWUNEL_IDENTITY_PROVIDER__1__DEFAULT="true"
```

Every field listed in the tables below has a matching environment variable. For
example, `trusted = true` in TOML becomes
`TUWUNEL_IDENTITY_PROVIDER__0__TRUSTED="true"`.

## Example configurartions

### GitHub

```toml
[[global.identity_provider]]
brand = "GitHub"
client_id = "Ov23liYourGitHubClientId"
client_secret = "your_github_client_secret"
callback_url = "https://matrix.example.com/_matrix/client/unstable/login/sso/callback/Ov23liYourGitHubClientId"
```

GitHub's `issuer_url` and `base_path` are pre-configured. `client_id` doubles
as the provider ID in the callback URL.

Tuwunel identifies a GitHub account by its numeric account `id`, not by its
`login`, which GitHub releases for anyone to register after a rename or account
deletion. An account that an earlier release linked by `login` moves to the
`id` at its next sign-in when the stored avatar URL names that `id`. Otherwise
link it again: if a sign-in already linked that GitHub `id` to another account,
remove that link with `query oauth delete <other_user_id> --force`, then run
[associate](#admin-approved-association-for-untrusted-providers) with
`--force --claim sub=<id>`. `--force` replaces the account's old link and also
removes its OAuth sessions with other providers.

### Google

```toml
[[global.identity_provider]]
brand = "Google"
client_id = "123456789-abc.apps.googleusercontent.com"
client_secret = "GOCSPX-your_secret"
callback_url = "https://matrix.example.com/_matrix/client/unstable/login/sso/callback/123456789-abc.apps.googleusercontent.com"
```

### Self-hosted Keycloak

```toml
[[global.identity_provider]]
brand = "Keycloak"
client_id = "tuwunel"
client_secret = "your_keycloak_secret"
issuer_url = "https://sso.example.com/realms/myrealm"
callback_url = "https://matrix.example.com/_matrix/client/unstable/login/sso/callback/tuwunel"
trusted = true
```

With `trusted = true`, users whose Keycloak username matches an existing Matrix
localpart are granted access to that account. Only use `trusted` when you
control the identity provider.

### Matrix Authentication Service (MAS)

```toml
[[global.identity_provider]]
brand = "MAS"
client_id = "01J44Q10GR4AMTFZEEF936DTCM"
client_secret = "<client_secret>"
issuer_url = "https://auth.example.com"
callback_url = "https://matrix.example.com/_matrix/client/unstable/login/sso/callback/01J44Q10GR4AMTFZEEF936DTCM"
```

MAS attaches as an upstream provider; Tuwunel remains its own next-generation
auth issuer. See the [MAS guide](providers/mas.md) for the full topology, the
client registration on the MAS side, and why `authorization_url` must stay
unset.

## Common setup patterns

### Linking existing users to an identity provider

When SSO is added to a server that already has password-based accounts, the
central question is: how does Tuwunel know which provider identity belongs to
which existing Matrix account?

The most direct approach is to set `trusted = true` and list `"sub"` in
`userid_claims`. The `sub` claim is the stable, globally unique user
identifier that every OIDC provider is required to maintain. Listing it in
`userid_claims` tells Tuwunel to use it as the authoritative match key.
Marking the provider as trusted then inverts Tuwunel's normal logic: instead
of registering a new account when it finds no existing match, it looks for an
existing Matrix account whose localpart equals the `sub` value and grants
access to it. The result is that logging in through the provider seamlessly
picks up the user's existing account:

```toml
[[global.identity_provider]]
brand = "Keycloak"
client_id = "tuwunel"
# ...
trusted = true
userid_claims = ["sub"]
```

For this to work, the `sub` value the provider returns for each user must
match that user's Matrix localpart exactly. If your provider allows you to
set `sub` to an arbitrary value, aligning it with the Matrix localpart is
the cleanest path. If it does not — for example, if `sub` is an opaque UUID
— you can use a different claim (such as `preferred_username`) as the match
key, or pre-register the link with the admin command described below.

**Only ever set `trusted = true` for identity providers you self-host and
fully control.** In trusted mode, anyone who can present a matching provider
identity gains access to the corresponding Matrix account. Public providers
such as GitHub and Google must never be trusted.

#### Admin-approved association for untrusted providers

When the provider is not trusted — a public service such as GitHub or Google
where `trusted = true` would be unsafe — you can still link an existing
Matrix account to a specific provider identity by having an admin pre-approve
the connection before the user's next login.

The admin command registers a set of claims to watch for from that provider.
When the user next authenticates, Tuwunel checks the claims returned by the
provider against any pending approvals. If every claim in the approval matches
what the provider returns, the accounts are linked and the approval is
consumed.

```
!admin query oauth associate <provider_id> @alice:example.com \
  --claim sub=550e8400-e29b-41d4-a716-446655440000
```

Specify whichever claims uniquely identify the user on that provider. `sub`
is the most reliable because every OIDC provider guarantees it is stable and
unique per user.

**Pending approvals are held in memory, not the database.** The affected user
must complete their login before the server is restarted, or the command must
be run again.

After a supported database migration, stored provider mappings can be adopted
in bulk from the source database. This one-time procedure has additional safety
requirements and must be completed before migrated users sign in. See
[Migrating Databases](migrating.md).

### Assigning accounts created before the first sign-in

A migration from another chat system can create each person's account ahead
of time, for example through an appservice's `/register` with
`m.login.application_service`, and fill it with history. Normally the person's
first sign-in then gets a new account, because an existing account that SSO
did not create is skipped. `assigned_accounts` names each person's account by
the address the provider sends in the `email` claim, so the provider must send
that claim (request the `email` scope):

```toml
[[global.identity_provider]]
# An app restricted to your own organization.
client_id = "tuwunel"
# ...
assigned_accounts = { "alice@example.com" = "@alice:example.com" }
```

**Use this with an app restricted to your own organization**, such as a Google
Workspace app with the "Internal" user type or a single-tenant Microsoft Entra
ID app, whose addresses the organization assigns; list only its members'
addresses (an Entra app also admits guests, whose addresses it does not assign).
Whoever controls a listed address gets the account and everything in it. Avoid
multi-tenant apps, and providers whose users can set or edit their own address
without verifying it (Keycloak, for one, lets users edit their email unless
verification is required). A sign-in whose provider says the address is
unverified (any `email_verified` value other than `true` or the string `"true"`
in any case) is refused. Many providers send that claim, such as Google,
Keycloak, Okta, Auth0, GitLab and Authentik; others, such as Microsoft Entra ID
and GitHub, do not, and an absent or null claim is not checked.

A listed account is reserved for its listed address: no other sign-in gets it by
name or as its fallback, at any provider (an admin association still can). Only
SSO honors the list, though: the other ways to create an account (registration,
LDAP, JWT login, the admin API and admin commands) do not, so create each listed
account before listing it, or a listed name that does not exist yet can be taken
through them. Do not combine `assigned_accounts` with `trusted = true` on one
provider all the same.

Each value must be a local user ID, and no address may be listed twice; the
server refuses to start otherwise. Addresses are compared ignoring ASCII case
only, so a lookalike non-ASCII character does not match. When
an identity with no account yet signs in with a listed address, it gets that
account and no other: the sign-in is refused, with a warning in the log naming
the reason, when the account does not exist, is the server user, is
deactivated, or is already linked to an identity (also one adopted from
another server's database). A chained sign-in through several default
providers is refused when a listed address names an account other than the
one the chain carries, unless that identity is already linked. Otherwise the
identity is linked to the account, which the log records. An account with an
empty password, such as one an appservice registered, becomes an SSO account
(origin `sso`, no password) in the same write that stores the link, with a
warning in the log; an account with a password or another
sign-in system's marker keeps it. The person's later sign-ins follow the link;
addresses not listed are mapped as described below. Joining rooms and setting
profiles is left to whatever created the accounts.

**List only accounts created for these people.** An account deactivated
before this server recorded deactivation reasons has an empty password and no
reason, looks like an account created without a password, and would be
reactivated. Remove the entries once their people have signed in or the
migration window ends. The configuration is the only record of the
assignments; a reload applies to the next sign-in.

### Letting only some people register

A provider open to the public, such as a Google app whose consent screen is
"External", lets any account it vouches for register. `registration_rules`
lets only some of them create an account, and joins each new account to its
rooms:

```toml
[[global.identity_provider]]
# ...
registration_rules = [
  { claims = { hd = "example.com" }, rooms = ["#lobby:example.com"] },
  { claims = { email = "alice@school.example" }, rooms = ["#class:example.com"], external = true },
]
```

A rule is met when every claim it names has its value. The claims are `sub`,
`preferred_username`, `username`, `nickname`, `login`, `name`, `given_name`,
`family_name`, `email` and `hd` (Google's Workspace domain, absent for consumer
accounts). `email` and `hd` are compared ignoring ASCII case, the others
exactly, and `email` counts only when the provider verified the address, as
described under [User ID mapping](#user-id-mapping). **Name only claims the
provider controls**, such as a verified `email`, `hd` or `sub`: people can set
their own `name` or `preferred_username` at many providers.

With rules, a sign-in that would create an account and meets none is refused,
and no account is made. That includes provisioning an LDAP user through a
provider with `registration = false`. Existing accounts sign in as before,
whatever the rules. A new account joins the rooms of every rule it meets, in
order and each once. When the room's join rule would refuse the account
uninvited, the room's highest-powered active local member that may invite
invites it first, so such a room needs a local member with that power. That
member is never the server's own user or an appservice (bridge) account, which
have no password, nor a locked, suspended or external account: a room whose
only possible inviter is such an account is skipped.

A room is a room ID or an alias on this server. **Prefer room IDs.** Any local
user may point an alias that nothing holds at any room, so an alias is used only
when it exists and the room it resolves to names it in its own
`m.room.canonical_alias` (as its alias or an alternative), which only the room's
members with power set; a room created with that alias name has it. So whoever
first creates a room with an alias a rule names that no room has yet owns a
room that rightly names it, and new accounts are invited there. The admin room
is never joined this way, by alias or ID: its members are server admins. Nor is
a room an upgrade replaced (one with an `m.room.tombstone`), such as an old
admin room. A skipped room, like a room this server is not in or any other
failed join, is logged and never fails the sign-in. The rooms are joined only on
the sign-in that creates the account: if that sign-in fails after the account
exists, later sign-ins join nothing and an admin invites. A reload applies to
the next sign-in.

#### External accounts

A Matrix `public` join rule lets any local account join, and a `restricted` one
lets in any account that is in one of the rooms it names, so join rules alone do
not keep accounts made for people outside the organization out of its rooms. A
rule with `external = true` marks the accounts it creates as external:

- They join only rooms they are invited to, by any route: `/join`, the state
  endpoint, an admin command. Leaving, knocking and accepting an invite work as
  usual; an approved knock is an invite. The rule's own rooms invite the account
  first, whatever their join rule.
- They create no rooms, upgrades included (the upgrade route refuses them
  before anything happens), no room aliases, and invite no one, third-party
  invites (`m.room.third_party_invite`) included.
  After a room upgrade they cannot join the replacement room by following the
  tombstone; someone in the new room invites them (the upgrade moves only the
  room's creator and the upgrading sender).
- Their user directory searches find only people they share a room with, even
  with `directory.users.show_all_local` (`show_all_local_users_in_user_directory`).
- They are not made the server's admin as its first account, and they join none
  of `auto_join_rooms`. An external account is never a server admin:
  `user_is_admin` is false for it even in the admin room, and the admin grant
  refuses it, so admin routes that act as the server user cannot carry it out of
  its rooms.

An account meeting several rules is external when any of them is. Admins can
still join an external account to a room by inviting it first, as the admin
join API (`POST /_synapse/admin/v1/join/{room}`) does; the `force-join-room`
admin command, which joins without an invite, is refused. **Do not list rooms
whose members gain privileges by membership** in a rule with `external = true`,
such as a room whose bots or integrations serve everyone in it: the rule invites
the account there like into any other room it lists. A client that joins a room
on its own, without an invite (such as a room the server advertises for shared
emoji), fails for an external account unless a rule invited it there. The mark
belongs to the account and is set when the rule creates it: later sign-ins and
rule changes neither set nor clear it. A failed read of the mark counts as
external, so a storage fault never lifts the limits.

#### Inviting one address

A server admin can let one more person register without editing the
configuration. A registration invite names an email address, the rooms its
account joins, and whether the account is external. It is in effect one more
rule for that address, `{ claims = { email = "..." }, rooms, external }`, that
is kept in the database, applies to every provider, and is used once.

Three requests manage the invites. Each needs the access token of a server
admin; another account's is refused with `M_FORBIDDEN`:

```sh
# Invite an address. `rooms` and `external` are optional: no rooms, not external.
curl -X PUT -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"rooms": ["!lobby:example.com"], "external": true}' \
  https://matrix.example.com/_matrix/client/unstable/io.mindroom/admin/registration_invites/alice@example.com

# List the invites.
curl -H "Authorization: Bearer $TOKEN" \
  https://matrix.example.com/_matrix/client/unstable/io.mindroom/admin/registration_invites

# Remove an invite.
curl -X DELETE -H "Authorization: Bearer $TOKEN" \
  https://matrix.example.com/_matrix/client/unstable/io.mindroom/admin/registration_invites/alice@example.com
```

`PUT` answers with the invite as it is stored,
`{"email": "alice@example.com", "rooms": ["!lobby:example.com"], "external": true}`,
and the list with `{"invites": [...]}`, by address. An address is stored in
lowercase, with `ß` as `ss`, so its case does not matter in a request or at the
provider, and writing it again replaces its invite. Percent-encode what a URL
path does not take as it is, such as `/`, `?`, `#` and `%`. `PUT` and `DELETE`
refuse a malformed address (`M_INVALID_PARAM`). `PUT` also refuses a room that
is neither a room ID nor an alias (`M_BAD_JSON`), an alias of another server
(`M_INVALID_PARAM`), and an address an account already has
(`M_THREEPID_IN_USE`, naming the account): only a sign-in that creates an
account uses an invite, so that one would stay in the list for good. `DELETE`
answers `M_NOT_FOUND` for an address without an invite. No room is checked to
exist when the invite is written: as with a rule, one that cannot be joined is
logged at the sign-in.

At sign-in:

- Only a sign-in that would create an account reads an invite, and only for an
  address the provider verified, as for a rule's `email` claim. An existing
  account signs in as before and leaves an invite for its address untouched.
- On a provider with rules, an invited address is admitted even when it meets
  no rule.
- The account is external when a rule it meets or the invite says so. It joins
  the rooms of the rules it meets, then the invite's, each once, exactly as
  described above for a rule's rooms.
- A provider without rules stays open to everyone. An invite there only gives
  its address the invite's rooms and external mark. `registration = false` is
  checked before the rules and the invites, and an invite does not lift it.
- The invite is deleted once the sign-in has succeeded. A sign-in that failed
  before that leaves it, so the person can try again.
- An invite that cannot be read fails the sign-in, so a storage fault never
  registers an invited external person as a normal account.

No email is sent: tell the person to sign in with that address. A provider
without rules admits everyone, so to admit invited people only, give it a rule
nobody meets, such as `{ claims = { hd = "nobody.invalid" } }`. A client can
tell that the server has invites from `io.mindroom.registration_invites` in the
`unstable_features` of `GET /_matrix/client/versions`, listed while an identity
provider is configured.

### How Tuwunel derives Matrix user IDs from claims

When a user authenticates through a provider for the first time and no
existing account is linked, Tuwunel must compute a Matrix localpart from the
claims in the provider's userinfo response. It tries each of the following in
order, using the first claim that is present and yields a valid, available
username:

1. `preferred_username`
2. `username`
3. `nickname`
4. `login`
5. `email` — only the portion before the `@` is used

The `sub` claim is deliberately excluded from this default sequence because
it is typically an opaque identifier rather than a human-readable name. It is
only consulted when explicitly listed in `userid_claims`, where it always
takes precedence over every other claim regardless of list order.

If none of these five claims appear in the userinfo response, or if every
derived candidate is already taken by another account, Tuwunel falls back to
a randomly generated localpart. This fallback is controlled by
`unique_id_fallbacks`, which defaults to `true`. On private servers where
silent random assignment is unacceptable, set it to `false` — Tuwunel will
return an error instead.

**Make sure user profiles on your identity provider contain at least one of
the five claims above.** If a profile has none of them, Tuwunel has nothing
to work with and will resort to the random fallback. The safest choice is to
ensure `preferred_username` is populated on every account, as it is the
first claim Tuwunel checks and tends to hold a recognisable, human-readable
name that also makes a reasonable Matrix localpart.

The `userid_claims` field lets you restrict which claims Tuwunel considers
and in what order. For example, to use only `preferred_username` and fall
back to the local part of the email address, and never silently generate a
random ID:

```toml
userid_claims = ["preferred_username", "email"]
unique_id_fallbacks = false
```

Listing `"sub"` anywhere in `userid_claims` elevates it to the highest
priority, overriding all other entries. The special value `"unique"` used
alone instructs Tuwunel to always generate a unique random localpart and
never attempt to derive one from claims at all.

### Registration hints for OAuth-aware clients

Clients that implement MSC3824 OAuth-aware login can append an `action`
parameter to the SSO redirect endpoint to signal whether the user means to log
in or to register:

```
/_matrix/client/v3/login/sso/redirect?action=register
```

Advertise that Tuwunel understands this flow by setting
`oidc_aware_preferred = true` under [Global SSO options](#global-sso-options).

Tuwunel only delegates the account screen to the provider: the hint is
honored by forwarding it upstream rather than by rendering a local page. Enable
`forward_action_prompt` on a provider and an incoming `action=register` is
translated to the OpenID Connect `prompt=create` parameter on that provider's
authorization request: the provider opens its sign-up screen instead of its
sign-in screen. `action=login` and a missing `action` are left untouched; a
`prompt` you configure through `extra_authorization_parameters` still applies in
those cases, while for `action=register` the derived `prompt=create` takes
precedence over it.

`prompt=create` comes from the OpenID Connect "Initiating User Registration"
extension, which not every provider implements. A provider that does not
support it may ignore the parameter or reject the request, so the option
defaults to off. Enable it only after confirming your provider advertises
`create` in the `prompt_values_supported` field of its OIDC discovery metadata.

## Multiple providers

When multiple providers are configured, each appears separately on the
client's login page (unless `single_sso = true`). The `default` field controls
which provider `/_matrix/client/v3/login/sso/redirect` (without a provider ID)
redirects to:

```toml
[[global.identity_provider]]
brand = "GitHub"
client_id = "github_client_id"
# ...
default = true   # this provider handles the bare SSO redirect

[[global.identity_provider]]
brand = "Google"
client_id = "google_client_id"
# ...
```

If no provider is explicitly `default` and exactly one is configured, it
becomes the implicit default.

## Global SSO options

These top-level options control how SSO providers are presented to clients.

| Option | Default | Description |
|---|---|---|
| `single_sso` | `false` | **(Experimental)** Replace the provider list with a single "Sign in with single sign-on" button at `/_matrix/client/v3/login/sso/redirect`. All providers are attempted in sequence and all must succeed. |
| `sso_custom_providers_page` | `false` | Replace the provider list with a single button and expect a reverse proxy to intercept `/_matrix/client/v3/login/sso/redirect` and serve a custom provider-selection page. Each entry on that page should link to `/_matrix/client/v3/login/sso/redirect/<client_id>`. |
| `oidc_aware_preferred` | `false` | Advertise OIDC as the preferred login method (MSC3824). Clients that support next-gen auth will present it as the only option. |

## Admin commands

These admin room commands help manage OAuth state:

| Command | Description |
|---|---|
| `!admin query oauth list-providers` | List all configured providers and their `provider_id`. |
| `!admin query oauth list-users` | List all users with an active OAuth session. |
| `!admin query oauth list-sessions [--user @user:example.com]` | List `session_id`, optionally filtered by user. |
| `!admin query oauth show-provider <provider_id>` | Show the active configuration for a provider. |
| `!admin query oauth show-user @user:example.com` | Show OAuth sessions for a user. |
| `!admin query oauth associate <provider_id> @user:example.com --claim key=value` | Associate an existing Matrix account with future OAuth claims from a provider. Useful for onboarding existing users to SSO. |
| `!admin query oauth adopt <provider_id>` | Adopt stored provider mappings in bulk after a supported database migration. See [Migrating Databases](migrating.md). |
| `!admin query oauth revoke <session_id\|@user:example.com>` | Revoke tokens for a session or all sessions of a user. |
| `!admin query oauth delete <session_id\|@user:example.com>` | Remove OAuth state entirely (destructive). Requires the `force` option. |

## Protocol flow reference

1. The client fetches `/_matrix/client/v3/login` and finds an `m.login.sso`
   entry listing configured providers.
2. The user selects a provider; the client redirects to
   `/_matrix/client/v3/login/sso/redirect/<client_id>`.
3. Tuwunel redirects the user to the provider's authorization endpoint.
4. The provider authenticates the user and redirects back to
   `/_matrix/client/unstable/login/sso/callback/<client_id>`.
5. Tuwunel exchanges the code for tokens, fetches user claims, maps them to a
   Matrix user ID, and issues a login token back to the client. Redirects on
   the configured client origin or the OIDC redirect allowlist continue
   directly. Other web and native clients require the user to confirm the
   destination first.

The public SSO redirect endpoint ignores `loginToken`. Provider chains carry
the authenticated account internally and require the browser grant cookie at
every chained step, even when ordinary single-provider cookie checks are
disabled for that provider.
