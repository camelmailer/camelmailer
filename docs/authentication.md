# Accounts, RBAC & SSO

CamelMailer's HTTP APIs accept three kinds of credentials:

| Credential | Header | Scope |
|---|---|---|
| Admin API key | `X-Admin-API-Key` | Machine-to-machine; full access to `/api/v2/admin` |
| Server API key | `X-Server-API-Key` | One mail server; `/api/v2/server` (sending, messages, stats) |
| **User session** | `Authorization: Bearer <token>` | A person; `/api/v2/auth` + `/api/v2/admin` subject to RBAC |

This document covers the third kind: user accounts with passwords,
two-factor authentication, organization roles, invitations, and OpenID
Connect single sign-on. Accounts require PostgreSQL (they are disabled on
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
`auth.lockout_minutes`), `TOTPRequired` (resubmit with `totp_code`),
`InvalidTOTPCode`.

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

> Password-reset tokens are delivered out of band: the operator finds the
> reset link in the web-server log (`password reset requested`). Set
> `auth.frontend_url` so the log carries a clickable frontend link.

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
  invitation_expiry_days: 7
  password_reset_expiry_hours: 2
  frontend_url: null              # e.g. https://mail-admin.example.com
```

## Deliberately deferred

SAML (OIDC is the supported enterprise SSO protocol — most IdPs speak
both), SCIM provisioning, WebAuthn/passkeys, per-user API scopes, and
app-mail delivery of reset/invitation links (tokens are currently handed
to the operator/frontend instead).
