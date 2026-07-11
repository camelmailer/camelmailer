# Accounts, RBAC & SSO

CamelMailer's HTTP APIs accept three kinds of credentials:

| Credential | Header | Scope |
|---|---|---|
| Admin API key | `X-Admin-API-Key` | Machine-to-machine; full access to `/api/v2/admin` |
| Server API key | `X-Server-API-Key` | One mail server; `/api/v2/server` (sending, messages, stats) |
| **User session** | `Authorization: Bearer <token>` | A person; `/api/v2/auth` + `/api/v2/admin` subject to RBAC |

This document covers the third kind: user accounts with passwords,
two-factor authentication, organization roles, invitations, single
sign-on (OpenID Connect and SAML), and SCIM provisioning. Accounts
require PostgreSQL (they are disabled on
non-persistent instances, where the endpoints answer `503
AccountsUnavailable`).

## Bootstrapping the first user

```bash
# generates and prints a password (or set CAMELMAILER_USER_PASSWORD):
docker compose exec web camelmailer make-user ops@example.com Ada Lovelace --admin
```

`--admin` makes a **global admin**: full access to every organization and
to the global resources (users, IP pools, admin API keys, the audit log).
Alternatively, create users through the admin API — `POST
/api/v2/admin/users` accepts an optional `password`.

## Login and sessions

```bash
curl -X POST http://localhost:5000/api/v2/auth/login \
  -H 'Content-Type: application/json' \
  -d '{"email_address": "ops@example.com", "password": "…"}'
# -> { "data": { "session_token": "…", "expires_at": "…", "user": {…} } }

curl http://localhost:5000/api/v2/auth/me -H "Authorization: Bearer <token>"
```

Sessions are opaque bearer tokens (stored hashed, SHA-256) with a sliding
expiry of `auth.session_timeout_days` (default 14). `POST /api/v2/auth/logout`
revokes the current session.

Error codes a frontend can branch on at login: `InvalidCredentials`,
`AccountLocked` (after `auth.max_login_attempts` failures, for
`auth.lockout_minutes`), `AccountDisabled` (deactivated, e.g. via SCIM),
`TOTPRequired` (resubmit with `totp_code`), `InvalidTOTPCode`.

The rest of the account surface:

| Endpoint | Purpose |
|---|---|
| `GET /api/v2/auth/me` | Profile, `totp_enabled`, memberships + roles |
| `PATCH /api/v2/auth/me` | Update first/last name |
| `POST /api/v2/auth/password` | Change password (requires current one); revokes all sessions, returns a fresh token |
| `POST /api/v2/auth/password-reset` | Request a reset (responds identically whether the address exists) |
| `POST /api/v2/auth/password-reset/complete` | `{token, new_password}` — single-use, expiring |
| `POST /api/v2/auth/totp/enroll` | Returns `secret` + `otpauth_url` for the authenticator app |
| `POST /api/v2/auth/totp/activate` | `{code}` — 2FA enforced only after this confirmation |
| `POST /api/v2/auth/totp/disable` | `{password}` |

Two-factor codes are standard RFC 6238 TOTP (SHA-1, 30 s, 6 digits) —
compatible with Google Authenticator, 1Password, Authy, etc.

> Without `app_mail` (see [Platform email delivery](#platform-email-delivery))
> password-reset tokens are delivered out of band: the operator finds the
> reset link in the web-server log (`password reset requested`). Set
> `auth.frontend_url` so the log carries a clickable frontend link. With
> `app_mail` enabled the link is emailed to the user instead and the token
> stays out of the log.

## Self-registration

Open sign-up is off by default. Set `auth.allow_registration: true`
(meant for public cloud offerings — self-hosters typically keep it off
and create accounts via invitations or `make-user`) and anyone can
create a regular, non-admin account:

```bash
curl -X POST http://localhost:5000/api/v2/auth/register \
  -H 'Content-Type: application/json' \
  -d '{"email_address": "grace@example.com", "first_name": "Grace",
       "last_name": "Hopper", "password": "…"}'
# -> 201, same shape as a login: { "data": { "session_token": "…", … } }
```

The new account is signed in immediately (the response matches the login
success response). While the flag is off the endpoint answers `403
RegistrationDisabled`. Other error codes: `ParameterMissing`,
`ValidationError` (invalid email, password shorter than
`auth.minimum_password_length`, or address already taken). Registrations
appear on the audit log as `registration.success`.

### Bootstrap workspace

With `auth.bootstrap_workspace: true` (off by default; meant for the
cloud, where a fresh account should be able to send mail immediately)
every **brand-new** account starts with a ready-made workspace:

- an organization **"\<FirstName>'s Team"** with the user as its owner
  (the permalink is the slugged name; collisions get a numeric suffix:
  `grace-s-team`, `grace-s-team-2`, `grace-s-team-3`, …),
- a server **`production`** inside it,
- and — registration only — an API credential **`default`**.

The register response then carries the workspace:

```json
{ "data": { "session_token": "…", "user": { … },
            "workspace": { "organization": "grace-s-team",
                           "server": "production",
                           "api_key": "…" } } }
```

`api_key` is shown **exactly once, here** — the usual
"secrets are shown once" convention. The same bootstrap runs when
OIDC/SAML/social-SSO auto-provisioning creates a new account (only on
the very first login that creates it), with one difference: SSO logins
have no response channel that could show a key once, so only the
organization and the server are created — **no** API credential. The
user creates one from the dashboard instead.

Bootstrap failures (e.g. no free permalink) never fail the registration
or SSO login: they are logged (`tracing::warn!`) and the response simply
carries no `workspace` — the account itself always survives.

## RBAC

A user's power inside an organization is its **membership role**:

| Action | viewer | member | admin | owner |
|---|---|---|---|---|
| Read organizations, servers, resources, member list | ✅ | ✅ | ✅ | ✅ |
| Manage server resources (domains, credentials, routes, webhooks, streams, templates, suppressions) | | ✅ | ✅ | ✅ |
| Create/update/delete/suspend servers | | | ✅ | ✅ |
| Manage members & invitations (non-owner roles) | | | ✅ | ✅ |
| Grant/change/remove **owner** roles | | | | ✅ |
| Delete the organization | | | | ✅ |

Additional rules:

- **Global admins** (`admin: true` on the account) and the machine
  `X-Admin-API-Key` bypass RBAC entirely.
- Global resources (`/users`, `/ip_pools`, `/admin_api_keys`,
  `/auth_events`) are global-admin only.
- Non-members receive the same `404` as for nonexistent organizations —
  membership checks don't leak which organizations exist.
- `GET /organizations` lists only the caller's organizations.
- Every organization keeps **at least one owner**: the last owner can be
  neither demoted nor removed.
- Any signed-in user may create an organization and becomes its owner;
  set `auth.allow_organization_creation: false` to restrict creation to
  global admins.

### Managing people

```text
GET    /api/v2/admin/organizations/{org}/members            list members + roles
POST   /api/v2/admin/organizations/{org}/members            add an existing account {email_address, role}
PATCH  /api/v2/admin/organizations/{org}/members/{user_id}  change a role {role}
DELETE /api/v2/admin/organizations/{org}/members/{user_id}  remove a member
GET    /api/v2/admin/organizations/{org}/invitations        list invitations
POST   /api/v2/admin/organizations/{org}/invitations        invite {email_address, role} -> invite_token (returned once)
DELETE /api/v2/admin/organizations/{org}/invitations/{id}   revoke
```

The invitation flow for people **without** an account:

1. An org admin creates the invitation; the response contains
   `invite_token` (exactly once) and, with `auth.frontend_url` set, a
   ready-made `invite_url`. Deliver it to the invitee.
2. `GET /api/v2/auth/invitations/{token}` (public) previews the
   invitation for the accept page.
3. `POST /api/v2/auth/invitations/accept` with `{token, first_name,
   last_name, password}` creates the account + membership and signs the
   new user in. For an **existing** account the membership is added but no
   session is issued — an invite link can never take over an account.

Invitations expire after `auth.invitation_expiry_days` (default 7) and are
single-use.

## Org-wide two-factor enforcement

Owners can require a second factor from everyone in an organization
(the Postmark pattern):

```text
PATCH /api/v2/admin/organizations/{org}   {"require_two_factor": true}
GET   /api/v2/admin/organizations/{org}   -> { …, "require_two_factor": true }
```

The `PATCH` is **owner-only** (admins get `403 Forbidden`). While the
flag is on, any user whose account has **no active second factor** —
neither activated TOTP nor at least one registered passkey — receives on
every resource of that organization:

```json
{ "status": "error", "error": { "code": "TwoFactorEnforced",
  "message": "This organization requires two-factor authentication. Enable two-factor authentication on your account to continue." } }
```

(HTTP 403.) The rules:

- The check sits in the central RBAC layer, so it covers the whole
  management surface of the organization — the organization itself,
  servers, domains, credentials, members, billing, everything below
  `/organizations/{org}`.
- It applies to **sessions only**. The machine `X-Admin-API-Key` is not
  a person and is unaffected.
- **Global admins are enforced too** — there is no backdoor. (That
  includes the owner who just enabled the flag without having 2FA:
  enable a second factor to get back in, or fix it via the admin key.)
- Non-members still get the usual indistinguishable `404`.
- Other organizations of the same user are untouched; the user's own
  account pages (`/api/v2/auth/*`) — including TOTP/passkey enrollment —
  always work, so users can enable 2FA and regain access immediately.

The web app shows affected users a full-page "Enable 2FA to access this
organization" card linking to Account → Security, and owners find the
toggle under Organization → Settings → Security.

## Passkeys (WebAuthn)

With `auth.webauthn` enabled, users can register **passkeys** (Touch ID,
Windows Hello, security keys, phone authenticators) on the Account →
Security page and sign in with them instead of a password:

```yaml
auth:
  webauthn:
    enabled: true
    rp_id: app.camelmailer.com          # the domain passkeys are scoped to
    rp_origin: https://app.camelmailer.com   # the exact browser origin
    rp_name: CamelMailer                # shown by the browser (optional)
```

`enabled` requires `rp_id` and `rp_origin`. Choose `rp_id` carefully:
passkeys are cryptographically bound to it, so **changing it later
invalidates every registered passkey**. `rp_origin` must be the exact
origin the frontend is served from (scheme + host + port).

The flow (all under `/api/v2/auth/webauthn`, JSON with binary fields as
unpadded base64url, exactly as the browser WebAuthn API produces them):

| Endpoint | Auth | Purpose |
|---|---|---|
| `POST /webauthn/register/start` | Bearer | Creation options for `navigator.credentials.create()`; existing passkeys are excluded |
| `POST /webauthn/register/finish` | Bearer | `{name, credential}` — verifies the attestation, stores the passkey |
| `GET /webauthn/credentials` | Bearer | List passkeys (name, created, last used — never key material) |
| `DELETE /webauthn/credentials/{id}` | Bearer | Remove a passkey (allowed even for the last one — the password remains) |
| `POST /webauthn/login/start` | none | `{email_address}` → request options for `navigator.credentials.get()` |
| `POST /webauthn/login/finish` | none | `{credential}` → session, same response as `POST /login` |

Security properties:

- While the feature is off, every endpoint answers `403 WebAuthnDisabled`.
- `login/start` answers with the **same generic shape** for unknown
  addresses (deterministic fake credential ids) — no user enumeration.
- Every `login/finish` failure is a generic `401 InvalidCredentials`;
  an account lockout (`AccountLocked`) applies to passkey logins too.
- Ceremony state lives server-side (like the OIDC login state), expires
  after five minutes and is strictly single-use.
- Signature counters are verified and persisted on every login —
  `webauthn-rs` rejects assertions from cloned authenticators.
- Successful logins appear on the audit log as `webauthn.login`,
  registrations as `webauthn.register`, removals as
  `webauthn.credential.delete`.

Frontends can discover whether to show the passkey button (plus the
sign-up link and the SSO button) via the public feature endpoint:

```bash
curl http://localhost:5000/api/v2/auth/features
# -> { "data": { "webauthn": true, "registration": false,
#                "oidc": { "enabled": false, "name": "OIDC" } } }
```

## Platform email delivery

CamelMailer can send its own account mail through its own sending
pipeline (dogfooding). Create a mail server on **this** installation with
a verified sending domain and an API credential, then configure:

```yaml
app_mail:
  enabled: true
  server_api_key: "<API credential of a mail server of this installation>"
  from_address: no-reply@example.com   # domain must be verified on that server
  from_name: CamelMailer               # optional display name
auth:
  frontend_url: https://mail-admin.example.com   # needed for the links
```

When enabled (both `server_api_key` and `from_address` are then
required), three mails are sent:

| Trigger | Mail |
|---|---|
| `POST /api/v2/auth/password-reset` | Reset link (`{frontend_url}/reset-password?token=…`) to the user — the token travels **only** in the mail, not in the log |
| `POST /api/v2/admin/organizations/{org}/invitations` | Accept link (`{frontend_url}/invitations/accept?token=…`) to the invitee — the response still returns `invite_token` to the inviting admin |
| `POST /api/v2/auth/register` | A short welcome mail (no token) |

There is no HTTP loopback: the key is resolved internally exactly like
messaging-API authentication (credential → server) and the mail is
enqueued through the same path as `POST /api/v2/server/messages` — it
shows up as a regular outgoing message of that server. Delivery problems
(invalid key, suspended server, unverified From domain) are logged via
`tracing` and **never** fail the triggering request; password resets fall
back to logging the link for the operator.

## Single sign-on (OIDC)

Any spec-compliant OpenID Connect provider works: Okta, Microsoft Entra
ID, Google Workspace, Keycloak, Authentik, … CamelMailer runs the
authorization-code flow with PKCE and validates ID tokens against the
provider's JWKS (`iss`, `aud`, `exp`, `nonce`).

```yaml
oidc:
  enabled: true
  name: "Okta"                       # shown on the login page
  issuer: https://acme.okta.com      # discovery: {issuer}/.well-known/openid-configuration
  identifier: "<client id>"
  secret: "<client secret>"
  # scopes: [openid, email, profile]
  # uid_field: sub                   # claim that permanently links the account
  # email_address_field: email
  # name_field: name
  # auto_provision: true             # create accounts on first SSO login
  # allowed_email_domains: [acme.com]
```

Register the redirect URI with your provider:

```text
{camelmailer.web_protocol}://{camelmailer.web_hostname}/api/v2/auth/oidc/callback
```

Flow endpoints:

- `GET /api/v2/auth/oidc/start` — redirects the browser to the provider
  (returns `{authorization_url}` as JSON when called with
  `Accept: application/json`).
- `GET /api/v2/auth/oidc/callback` — completes the login. With
  `auth.frontend_url` configured the session token is handed to
  `{frontend_url}/auth/callback#session_token=…` in the URL fragment;
  without it the callback answers JSON.

Accounts resolve in order: already linked by the `uid_field` claim →
existing account with the same email (gets linked) → provisioned fresh
(when `auto_provision` allows it and the email domain passes
`allowed_email_domains`). SSO users can also be pre-created via the admin
API and need no password.

The upstream Postal `oidc` group uses the same field names, so a legacy
`postal.yml` loads unchanged.

## Social sign-in (Google / Microsoft / GitHub)

Alongside the single enterprise `oidc` group, any number of **social
sign-in providers** can be configured. Each renders as a
"Continue with {name}" button on the login page (and "Sign up with
{name}" on the registration page — social sign-in provisions accounts
automatically, subject to the per-provider policy below):

```yaml
auth:
  sso_providers:
    - { id: google,    type: oidc,   name: Google,
        issuer: "https://accounts.google.com",
        client_id: "…", client_secret: "…" }
    - { id: microsoft, type: oidc,   name: Microsoft,
        issuer: "https://login.microsoftonline.com/common/v2.0",
        client_id: "…", client_secret: "…" }
    - { id: github,    type: github, name: GitHub,
        client_id: "…", client_secret: "…" }
```

`id` is a unique slug (lowercase letters, digits, hyphens) and becomes
part of the endpoints. Register this **redirect URI** with each
provider (Google Cloud Console, Microsoft Entra app registration,
GitHub OAuth app):

```text
https://<host>/api/v2/auth/sso/{id}/callback
# concretely: {camelmailer.web_protocol}://{camelmailer.web_hostname}/api/v2/auth/sso/{id}/callback
```

Per provider, optionally:

- `allowed_email_domains: [corp.example]` — restrict who may sign in /
  provision.
- `auto_provision: false` — only accounts that already exist (by email)
  may sign in (default `true`).

Flow endpoints (per provider, mirroring the enterprise OIDC pair):

- `GET /api/v2/auth/sso/{id}/start` — redirects the browser to the
  provider (JSON `{authorization_url}` with `Accept: application/json`).
- `GET /api/v2/auth/sso/{id}/callback` — completes the login exactly
  like the OIDC callback (frontend fragment redirect or JSON). An
  unknown `{id}` answers `404 SSOProviderNotFound`.

`GET /api/v2/auth/features` (public) tells a frontend what to render:
`{ "sso": [{ "id", "name", "type" }] }` — never secrets.

Provider notes:

- **`type: oidc`** runs the same authorization-code flow with PKCE and
  full ID-token validation (signature via the issuer's JWKS, `iss`,
  `aud`, `exp`, `nonce`) as the enterprise OIDC module; the account
  links by the `sub` claim, `email`/`name` fill the profile.
- **Microsoft multi-tenant**: with the `common` issuer
  (`https://login.microsoftonline.com/common/v2.0`) the token's `iss`
  names the user's tenant (`…/{tenantid}/v2.0`), so it cannot equal the
  configured string. For issuers ending in `/common/v2.0` the `iss`
  claim is instead validated against the pattern
  `https://login.microsoftonline.com/<tenant-guid>/v2.0`; the
  signature (Microsoft's common JWKS covers all tenants), `aud`, `exp`
  and `nonce` checks stay hard. Single-tenant apps should configure
  their tenant issuer and get strict `iss` equality.
- **`type: github`** uses GitHub's plain OAuth2 code flow (GitHub does
  not implement OIDC). After the exchange, `/user` and `/user/emails`
  supply the identity: the primary **verified** email address (falling
  back to any verified one) becomes the account email — an account
  without a verified email is rejected with `422 SSOEmailUnavailable`.
  The display name comes from the profile `name`, falling back to the
  `login`. The account links by the numeric GitHub user id.

Account resolution and auditing work exactly like the enterprise OIDC
flow: already linked (per provider) → existing account with the same
email gets linked → provisioned fresh (`sso.login` / `sso.provision`
audit events). One account can hold links to several providers at once
— each provider keeps its own stable subject.
## SAML

For identity providers that only speak SAML 2.0 (or where the SAML app
catalog entry is the paved path), CamelMailer acts as a SAML service
provider: HTTP-Redirect binding for the `AuthnRequest`, HTTP-POST
binding for the response.

```yaml
saml:
  enabled: true
  name: "Okta"                          # login-page button label
  idp_sso_url: https://acme.okta.com/app/.../sso/saml
  idp_certificate: |                    # the IdP signing certificate —
    -----BEGIN CERTIFICATE-----         # inline PEM or a file path
    …
    -----END CERTIFICATE-----
  # sp_entity_id: https://mail.example.com   # default: {web_protocol}://{web_hostname}
  # auto_provision: true                # create accounts on first login
  # allowed_email_domains: [acme.com]
```

Register CamelMailer with the IdP using the SP metadata:

```text
GET  {web}/api/v2/auth/saml/metadata   SP metadata XML (entity id + ACS)
GET  {web}/api/v2/auth/saml/start      begins the login (redirect to the IdP;
                                       JSON {authorization_url, name} with
                                       Accept: application/json)
POST {web}/api/v2/auth/saml/acs        assertion consumer service — the
                                       IdP posts the response here
```

After a successful login the ACS behaves exactly like the OIDC
callback: with `auth.frontend_url` set the browser is redirected to
`{frontend_url}/auth/callback#session_token=…`, otherwise the session
is returned as JSON. When `saml.enabled` is off the endpoints answer
`404 SAMLDisabled`.

Every response is fully validated before a session is issued — there
is no way to turn any of these checks off:

- the XML signature (on the assertion, or on the response enveloping
  it) is verified against the **configured** `idp_certificate` only;
  unsigned responses, `rsa-sha1` signatures and keys supplied in the
  message (`ds:KeyInfo`) are rejected
- `Audience` must be the SP entity id; `Destination` and the bearer
  `SubjectConfirmationData` `Recipient` (when present) must be the ACS
  URL
- `InResponseTo` must redeem the id of an `AuthnRequest` this instance
  issued in the last 10 minutes — single use, so IdP-initiated
  (unsolicited) logins are rejected
- `NotBefore`/`NotOnOrAfter` on the Conditions and the subject
  confirmation are enforced with ±90 s clock skew
- the assertion id enters a replay cache until its `NotOnOrAfter`;
  presenting the same assertion twice fails

The signed-in identity is the email address: the `NameID` in
`emailAddress` format, or the usual email attribute (`email`, `mail`,
`urn:oid:0.9.2342.19200300.100.1.3`). Given/family name come from
`givenName`/`sn`-style attributes with a display-name fallback.
Accounts resolve by email: existing account → signed in; unknown →
provisioned (when `auto_provision` allows it and the domain passes
`allowed_email_domains`). Logins and provisioning appear on the audit
log as `saml.login` / `saml.provision`.

## SCIM provisioning

SCIM 2.0 (RFC 7643/7644, Users core) lets Okta, Entra ID & co. create,
update and deactivate CamelMailer accounts automatically:

```yaml
scim:
  enabled: true
  bearer_token: "<long random secret>"   # required when enabled
```

The SCIM surface lives under `/scim/v2` (own conventions: bearer-token
auth, `application/scim+json`, SCIM error format — not the `{ status,
time, … }` envelope):

| Endpoint | Purpose |
|---|---|
| `GET /scim/v2/ServiceProviderConfig` | capabilities (PATCH yes, bulk no, filter `userName eq`) |
| `GET /scim/v2/ResourceTypes`, `GET /scim/v2/Schemas` | discovery |
| `GET /scim/v2/Users` | list; `startIndex`/`count` pagination, `filter=userName eq "…"` |
| `POST /scim/v2/Users` | create — `userName` **is** the email address; duplicate → `409` (`scimType: uniqueness`) |
| `GET /scim/v2/Users/{id}` | read |
| `PUT /scim/v2/Users/{id}` | replace `userName`, `name`, `active` |
| `PATCH /scim/v2/Users/{id}` | RFC 7644 PatchOp: `active`, `userName`, `name.givenName`, `name.familyName` |
| `DELETE /scim/v2/Users/{id}` | **deactivates** (never hard-deletes — memberships and audit history survive) |

Authenticate every request with `Authorization: Bearer
<scim.bearer_token>`; the token is compared in constant time and a
wrong or missing token answers `401` in the SCIM error format.

`active: false` deactivates the account: all sessions are revoked
immediately and the account can no longer sign in — password login and
password-reset completion answer `403 AccountDisabled`, and OIDC/SAML
logins are refused the same way. `active: true` reactivates it. SCIM
changes appear on the audit log as `scim.provision`,
`scim.deactivate` and `scim.reactivate`.

SCIM-provisioned accounts have no password; users sign in through SSO,
or set a password via the reset flow.

## CORS

For a browser frontend calling the APIs directly:

```yaml
web_server:
  cors_origins:
    - https://mail-admin.example.com
    # - "*"        # any origin
```

Empty (default) sends no CORS headers. Allowed request headers cover
`Authorization`, `Content-Type`, `X-Admin-API-Key` and
`X-Server-API-Key`. The APIs are cookie-free (Bearer tokens only), so no
credentialed CORS is involved and CSRF does not apply.

## Audit log

Every authentication event — logins (success/failure/lockout), logouts,
password changes and resets, TOTP changes, SSO logins/provisioning,
membership and invitation changes — is recorded with IP address and user
agent:

```bash
curl http://localhost:5000/api/v2/admin/auth_events?limit=100 \
  -H "Authorization: Bearer <global-admin-token>"
```

## Configuration reference

```yaml
auth:
  session_timeout_days: 14        # sliding session lifetime
  max_login_attempts: 5           # failures before lockout
  lockout_minutes: 15
  minimum_password_length: 8      # must be >= 8
  allow_organization_creation: true
  allow_registration: false     # open self-registration (POST /api/v2/auth/register)
  bootstrap_workspace: false    # auto-create org + server (+ API key on register)
                                # for brand-new accounts — meant for the cloud
  invitation_expiry_days: 7
  password_reset_expiry_hours: 2
  frontend_url: null              # e.g. https://mail-admin.example.com
  sso_providers: []               # social sign-in (see above)
  webauthn:                       # passkeys (see above)
    enabled: false
    rp_id: null                   # required when enabled
    rp_origin: null               # required when enabled
    rp_name: CamelMailer

app_mail:                         # platform email delivery (see above)
  enabled: false
  server_api_key: null            # required when enabled
  from_address: null              # required when enabled
  from_name: CamelMailer

saml:                             # SAML 2.0 SSO (see above)
  enabled: false
  name: SAML                      # login-page button label
  idp_sso_url: null               # required when enabled
  idp_certificate: null           # required when enabled (PEM or path)
  sp_entity_id: null              # default {web_protocol}://{web_hostname}
  auto_provision: true
  allowed_email_domains: []

scim:                             # SCIM 2.0 provisioning (see above)
  enabled: false
  bearer_token: null              # required when enabled
```

## Deliberately deferred

SAML (OIDC is the supported enterprise SSO protocol — most IdPs speak
both), SCIM provisioning, and per-user API scopes.
WebAuthn/passkeys and per-user API scopes.
