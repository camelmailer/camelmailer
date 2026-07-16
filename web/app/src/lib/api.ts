// Typed client for the CamelMailer HTTP APIs.
//
// Every response uses the `{ status, time, data | error }` envelope; this
// client unwraps `data` and throws `ApiError` (carrying the backend error
// `code`, e.g. InvalidCredentials / TOTPRequired / Forbidden) otherwise.
//
// Auth: user sessions send `Authorization: Bearer <token>`; the messaging
// endpoints of a mail server send `X-Server-API-Key` instead.

const BASE_URL: string = process.env.NEXT_PUBLIC_API_URL ?? ""

const TOKEN_KEY = "camelmailer.session_token"

export function getToken(): string | null {
  if (typeof window === "undefined") return null
  return localStorage.getItem(TOKEN_KEY)
}
export function setToken(token: string | null) {
  if (typeof window === "undefined") return
  if (token === null) localStorage.removeItem(TOKEN_KEY)
  else localStorage.setItem(TOKEN_KEY, token)
}

export class ApiError extends Error {
  code: string
  status: number
  constructor(code: string, message: string, status: number) {
    super(message)
    this.code = code
    this.status = status
  }
}

type Envelope<T> = {
  status: "success" | "error"
  time: number
  data?: T
  error?: { code: string; message: string }
}

async function request<T>(
  method: string,
  path: string,
  body?: unknown,
  headers: Record<string, string> = {},
): Promise<T> {
  const token = getToken()
  const response = await fetch(`${BASE_URL}${path}`, {
    method,
    headers: {
      ...(body !== undefined ? { "Content-Type": "application/json" } : {}),
      ...(token && !headers["X-Server-API-Key"]
        ? { Authorization: `Bearer ${token}` }
        : {}),
      ...headers,
    },
    body: body !== undefined ? JSON.stringify(body) : undefined,
  })
  let envelope: Envelope<T>
  try {
    envelope = (await response.json()) as Envelope<T>
  } catch {
    throw new ApiError("NetworkError", `HTTP ${response.status}`, response.status)
  }
  if (envelope.status === "error" || !response.ok) {
    const error = envelope.error ?? { code: "UnknownError", message: "Unknown error" }
    throw new ApiError(error.code, error.message, response.status)
  }
  return envelope.data as T
}

export const api = {
  get: <T>(path: string, headers?: Record<string, string>) =>
    request<T>("GET", path, undefined, headers),
  post: <T>(path: string, body?: unknown, headers?: Record<string, string>) =>
    request<T>("POST", path, body, headers),
  patch: <T>(path: string, body?: unknown, headers?: Record<string, string>) =>
    request<T>("PATCH", path, body, headers),
  delete: <T>(path: string, headers?: Record<string, string>) =>
    request<T>("DELETE", path, undefined, headers),
}

// ------------------------------------------------------------------ types

export type User = {
  id: number
  uuid: string
  email_address: string
  first_name: string
  last_name: string
  admin: boolean
  totp_enabled?: boolean
}

export type Organization = {
  id: number
  uuid: string
  name: string
  permalink: string
  /** Org-wide 2FA enforcement: members without TOTP or a passkey get
   *  403 TwoFactorEnforced on every org resource. Owner-only setting. */
  require_two_factor: boolean
}

export type Role = "viewer" | "member" | "admin" | "owner"

// Billing (hosted cloud). `enabled: false` on self-hosted installations —
// the UI hides billing entirely in that case.
export type BillingStatus = { enabled: boolean; has_customer: boolean }

export type Membership = {
  role: Role
  created_at: string
  user: User
}

export type MeResponse = {
  user: User
  memberships: { role: Role; organization: Organization }[]
}

// Tenant SSO: an email domain the organization claims for login routing,
// with the DNS TXT challenge that proves ownership.
export type OrgSsoDomain = {
  id: number
  domain: string
  verified: boolean
  created_at: string
  dns_record: DnsRecord
}

export type OrgSsoKind = "oidc" | "saml" | "google" | "microsoft" | "github"

// One per-organization SSO connection. `config` holds the protocol
// fields; secrets come back masked and an unchanged mask keeps the
// stored secret on update.
export type OrgSsoConnection = {
  id: number
  kind: OrgSsoKind
  name: string
  enabled: boolean
  config: Record<string, string>
  default_role: Role
  auto_provision: boolean
  created_at: string
}

// A connection offered to a signed-out user after email discovery.
export type DiscoveredSsoConnection = {
  id: number
  kind: OrgSsoKind
  name: string
  start_url: string
}

export type Server = {
  id: number
  uuid: string
  name: string
  permalink: string
  mode: "Live" | "Development"
  suspended: boolean
  suspension_reason: string | null
  privacy_mode: boolean
  track_opens: boolean
  track_clicks: boolean
  spam_threshold: number | null
  outbound_spam_threshold: number | null
  bounce_hook_url: string | null
  delivery_hook_url: string | null
  inbound_domain: string | null
  color: string | null
  ip_pool_id: number | null
  default_stream_id: number | null
  // Physical postal address shown in the CAN-SPAM footer of broadcast mail.
  broadcast_physical_address: string | null
}

/// Per-server 30-day message counters (GET .../servers/stats). `server`
/// is the server permalink; counts are over the last 30 days.
export type ServerStat = {
  server: string
  total: number
  outgoing: number
  incoming: number
  bounced: number
}

/// Full windowed message counters (GET .../servers/{server}/stats and the
/// per-server /api/v2/server/stats). `bounces` breaks the bounced total
/// into hard/soft/undetermined.
export type WindowStats = {
  total: number
  incoming: number
  outgoing: number
  sent: number
  pending: number
  held: number
  bounced: number
  soft_fail: number
  hard_fail: number
  opens: number
  clicks: number
  unique_opens: number
  unique_clicks: number
  bounces?: { hard: number; soft: number; undetermined: number }
}

export type DnsRecord = { name: string; type: string; value: string }

export type Domain = {
  id: number
  uuid: string
  name: string
  verified: boolean
  // null when neither the domain nor the installation has a DKIM key
  dkim_record: DnsRecord | null
  verification_record: DnsRecord
  spf_record: DnsRecord
}

export type Credential = {
  id: number
  uuid: string
  type: "SMTP" | "API" | "SMTP-IP"
  name: string
  key: string
  hold: boolean
}

export type Route = {
  id: number
  uuid: string
  name: string
  mode: "Endpoint" | "Accept" | "Hold" | "Bounce" | "Reject"
  domain_id: number | null
  endpoint_url: string | null
  token: string
}

export type Webhook = {
  id: number
  uuid: string
  name: string
  url: string
  all_events: boolean
  enabled: boolean
  sign: boolean
  /** Subscribed event names; empty = all events. */
  events: string[]
  /** Extra HTTP headers sent with every delivery request. */
  headers: Record<string, string>
}

export const WEBHOOK_EVENTS = [
  "MessageSent",
  "MessageDelayed",
  "MessageDeliveryFailed",
  "MessageHeld",
] as const

export type SenderAddress = {
  id: number
  uuid: string
  email_address: string
  verified: boolean
  status: "pending" | "confirmed"
}

export type Suppression = {
  id: number
  type: string
  address: string
  reason: string | null
  // null = server-wide (hard bounces, manual); a set id scopes the opt-out
  // to one message stream (e.g. a broadcast unsubscribe).
  stream_id: number | null
}

/// The stream lookup returned alongside a suppression list, so the UI can
/// name each suppression's scope without a second (messaging-API) call.
export type SuppressionStreamRef = {
  id: number
  name: string
  permalink: string
  stream_type: string
}

export type Invitation = {
  id: number
  uuid: string
  email_address: string
  role: Role
  expires_at: string
  accepted_at: string | null
  invite_token?: string
  invite_url?: string
}

export type IpPool = { id: number; uuid: string; name: string; default: boolean }
export type IpAddress = {
  id: number
  uuid: string
  ipv4: string
  ipv6: string | null
  hostname: string
  priority: number
}
export type AdminApiKey = {
  id: number
  uuid: string
  name: string
  key_prefix: string
  key?: string
}
export type AuthEvent = {
  id: number
  user_id: number | null
  email_address: string | null
  event: string
  ip_address: string | null
  user_agent: string | null
  created_at: string
}

export type Features = {
  webauthn: boolean
  registration: boolean
  oidc: { enabled: boolean; name: string }
  saml: { enabled: boolean; name: string }
  sso: SsoProviderInfo[]
  // Hosted-cloud legal links, shown on the auth cards when configured.
  legal: { terms_url: string | null; privacy_url: string | null }
}

export type PasskeyCredential = {
  id: number
  name: string
  created_at: string
  last_used_at: string | null
}

export type Pagination = { page: number; per_page: number; total: number; total_pages: number }

// DMARC monitoring
export type HealthStatus = "ok" | "warning" | "missing"

export type DomainHealthCheck = {
  status: HealthStatus
  record_name: string
  found: string[]
  expected: string | null
  problems: string[]
}

export type DomainHealth = {
  domain: string
  checks: {
    spf: DomainHealthCheck
    dkim: DomainHealthCheck
    dmarc: DomainHealthCheck & {
      policy: { p: string | null; sp: string | null; rua: string[]; pct: number } | null
    }
  }
  overall: HealthStatus
  next_step: string
  rua_address: string | null
  compliance: {
    window_days: number
    total: number
    pass: number
    pass_rate: number
  } | null
}

export type DmarcSummary = {
  total: number
  pass: number
  fail: number
  pass_rate: number
  by_source: {
    source_ip: string
    count: number
    spf_aligned_pct: number
    dkim_aligned_pct: number
    disposition_counts: Record<string, number>
  }[]
  by_disposition: Record<string, number>
}

export type DmarcReport = {
  id: number
  domain: string
  org_name: string | null
  org_email: string | null
  report_id: string
  date_range_begin: string
  date_range_end: string
  received_at: string
  record_count: number
}

export type DmarcRecord = {
  id: number
  source_ip: string
  count: number
  disposition: string
  dkim_result: string | null
  spf_result: string | null
  dkim_aligned: boolean
  spf_aligned: boolean
  header_from: string | null
  envelope_from: string | null
}

// server API (X-Server-API-Key) types
export type Message = {
  id: number
  token?: string
  message_id: string | null
  scope: string
  rcpt_to: string
  mail_from: string | null
  subject?: string | null
  tag?: string | null
  status?: string | null
  spam_status?: string | null
  spam_score?: number | null
  threat?: boolean
  size?: number | null
  stream_id?: number | null
  held?: boolean
  bounce?: boolean
  bypassed?: boolean
  created_at: string
  metadata?: unknown
}

export type Delivery = {
  id: number
  status: string
  details: string | null
  output: string | null
  sent_with_ssl?: boolean
  timestamp: string
}

export type Stats = {
  total: number
  incoming: number
  outgoing: number
  sent: number
  pending: number
  held: number
  bounced: number
  soft_fail: number
  hard_fail: number
  opens: number
  clicks: number
  unique_opens: number
  unique_clicks: number
}

export type Stream = {
  id: number
  uuid: string
  name: string
  permalink: string
  stream_type: string
  archived: boolean
  // Reputation isolation: the IP pool this stream sends from (null = the
  // server's pool).
  ip_pool_id: number | null
}

/// An opt-in subscriber of a (broadcast) stream.
export type Subscription = {
  id: number
  address: string
  status: "subscribed" | "unsubscribed"
  created_at: string | null
}

export type Template = {
  id: number
  uuid: string
  name: string
  permalink: string
  subject: string | null
  html_body: string | null
  text_body: string | null
  archived: boolean
  layout_id: number | null
}

// A reusable wrapper around template bodies (logo, address, social links).
// The HTML wrapper embeds the rendered body via {{{ content }}}.
export type Layout = {
  id: number
  uuid: string
  name: string
  permalink: string
  html_wrapper: string
  text_wrapper: string | null
}

// message share links + deliverability insights
export type MessageShare = { url: string; expires_at: string }

export type SharedMessage = {
  message: Message
  deliveries: Delivery[]
  opens: { ip_address: string | null; user_agent: string | null; created_at: string }[]
  clicks: {
    ip_address: string | null
    user_agent: string | null
    url: string | null
    created_at: string
  }[]
  html_body: string | null
  text_body: string | null
  expires_at: string
}

export type InsightCheck = {
  id: string
  title: string
  status: "ok" | "warning"
  detail: string
}

export type MessageInsights = { generated_at: string; checks: InsightCheck[] }

export type WebhookTestResult = {
  delivered: boolean
  status_code: number | null
  duration_ms: number
  error?: string
}

/** Resolve a public share link — no authentication. */
export const shareApi = {
  message: (token: string) =>
    api.get<SharedMessage>(`/api/v2/share/messages/${encodeURIComponent(token)}`),
}

// -------------------------------------------------------------- auth API

export const authApi = {
  login: (email_address: string, password: string, totp_code?: string) =>
    api.post<{ session_token: string; expires_at: string; user: User }>(
      "/api/v2/auth/login",
      { email_address, password, ...(totp_code ? { totp_code } : {}) },
    ),
  // 403 RegistrationDisabled unless auth.allow_registration is on.
  // With auth.bootstrap_workspace (cloud) the response also carries a
  // ready-made workspace; its api_key is shown exactly once, here.
  register: (fields: {
    email_address: string
    first_name: string
    last_name: string
    password: string
  }) =>
    api.post<{
      session_token: string
      expires_at: string
      user: User
      workspace?: { organization: string; server: string; api_key: string }
    }>("/api/v2/auth/register", fields),
  logout: () => api.post<{ logged_out: boolean }>("/api/v2/auth/logout"),
  me: () => api.get<MeResponse>("/api/v2/auth/me"),
  updateMe: (fields: { first_name?: string; last_name?: string }) =>
    api.patch<{ user: User }>("/api/v2/auth/me", fields),
  changePassword: (current_password: string, new_password: string) =>
    api.post<{ password_changed: boolean; session_token: string }>(
      "/api/v2/auth/password",
      { current_password, new_password },
    ),
  requestReset: (email_address: string) =>
    api.post<{ reset_requested: boolean }>("/api/v2/auth/password-reset", { email_address }),
  completeReset: (token: string, new_password: string) =>
    api.post<{ password_reset: boolean }>("/api/v2/auth/password-reset/complete", {
      token,
      new_password,
    }),
  totpEnroll: () =>
    api.post<{ secret: string; otpauth_url: string }>("/api/v2/auth/totp/enroll"),
  totpActivate: (code: string) =>
    api.post<{ totp_enabled: boolean }>("/api/v2/auth/totp/activate", { code }),
  totpDisable: (password: string) =>
    api.post<{ totp_enabled: boolean }>("/api/v2/auth/totp/disable", { password }),
  invitationPreview: (token: string) =>
    api.get<{
      invitation: {
        email_address: string
        role: Role
        expires_at: string
        organization: { name: string; permalink: string } | null
        user_exists: boolean
      }
    }>(`/api/v2/auth/invitations/${token}`),
  invitationAccept: (fields: {
    token: string
    first_name?: string
    last_name?: string
    password?: string
  }) =>
    api.post<{
      accepted: boolean
      account_created: boolean
      user: User
      session_token?: string
    }>("/api/v2/auth/invitations/accept", fields),
  confirmSenderAddress: (token: string) =>
    api.post<{ confirmed: boolean; email_address: string }>(
      "/api/v2/auth/sender-addresses/confirm",
      { token },
    ),
  // 404 SSODisabled when OIDC is off — used to decide whether to render
  // the SSO button.
  oidcStartUrl: () =>
    api.get<{ authorization_url: string }>("/api/v2/auth/oidc/start", {
      Accept: "application/json",
    }),
  // Which optional login features this instance exposes (passkeys,
  // self-registration, OIDC, social sign-in) — unauthenticated, drives
  // the login page.
  features: () => api.get<Features>("/api/v2/auth/features"),
  // Tenant SSO discovery: which connections apply to this email's domain.
  // Empty when the domain is not verified by any organization.
  orgSsoDiscover: (email: string) =>
    api.post<{ connections: DiscoveredSsoConnection[] }>("/api/v2/auth/org-sso/discover", {
      email,
    }),
  // 404 SAMLDisabled when SAML is off — used to decide whether to render
  // the "Sign in with <name>" button.
  samlStartUrl: () =>
    api.get<{ authorization_url: string; name: string }>(
      "/api/v2/auth/saml/start",
      { Accept: "application/json" },
    ),
  // WebAuthn / passkeys. Binary fields inside the options/credential
  // payloads are unpadded base64url — see src/lib/webauthn.ts for the
  // browser-API conversion. 403 WebAuthnDisabled while the feature is off.
  webauthnRegisterStart: () =>
    api.post<{ publicKey: Record<string, unknown> }>(
      "/api/v2/auth/webauthn/register/start",
      {},
    ),
  webauthnRegisterFinish: (name: string, credential: unknown) =>
    api.post<{ credential: PasskeyCredential }>(
      "/api/v2/auth/webauthn/register/finish",
      { name, credential },
    ),
  webauthnCredentials: () =>
    api.get<{ credentials: PasskeyCredential[] }>("/api/v2/auth/webauthn/credentials"),
  webauthnDeleteCredential: (id: number) =>
    api.delete<{ deleted: boolean }>(`/api/v2/auth/webauthn/credentials/${id}`),
  webauthnLoginStart: (email_address: string) =>
    api.post<{ publicKey: Record<string, unknown> }>(
      "/api/v2/auth/webauthn/login/start",
      { email_address },
    ),
  webauthnLoginFinish: (credential: unknown) =>
    api.post<{ session_token: string; expires_at: string; user: User }>(
      "/api/v2/auth/webauthn/login/finish",
      { credential },
    ),
}

export type SsoProviderInfo = {
  id: string
  name: string
  type: "oidc" | "github"
}

/** The browser-facing entry point of a social sign-in provider. */
export function ssoStartUrl(providerId: string): string {
  return `/api/v2/auth/sso/${providerId}/start`
}

// ------------------------------------------------------------- admin API

export const adminApi = {
  organizations: {
    list: () =>
      api.get<{ organizations: Organization[]; pagination: Pagination }>(
        "/api/v2/admin/organizations?per_page=100",
      ),
    create: (name: string) =>
      api.post<{ organization: Organization }>("/api/v2/admin/organizations", { name }),
    get: (permalink: string) =>
      api.get<{ organization: Organization }>(`/api/v2/admin/organizations/${permalink}`),
    // Owner-only; currently the one mutable setting is require_two_factor.
    update: (permalink: string, fields: { require_two_factor?: boolean }) =>
      api.patch<{ organization: Organization }>(
        `/api/v2/admin/organizations/${permalink}`,
        fields,
      ),
    delete: (permalink: string) =>
      api.delete<{ deleted: boolean }>(`/api/v2/admin/organizations/${permalink}`),
  },
  billing: (org: string) => ({
    // 200 with enabled=false when billing is off (self-hosted) — never an error.
    get: () => api.get<BillingStatus>(`/api/v2/admin/organizations/${org}/billing`),
    // 403 BillingDisabled when off; 502 BillingUnavailable when Stripe is down.
    portal: () => api.post<{ url: string }>(`/api/v2/admin/organizations/${org}/billing/portal`),
  }),
  members: (org: string) => ({
    list: () => api.get<{ members: Membership[] }>(`/api/v2/admin/organizations/${org}/members`),
    add: (email_address: string, role: Role) =>
      api.post<{ member: Membership }>(`/api/v2/admin/organizations/${org}/members`, {
        email_address,
        role,
      }),
    setRole: (userId: number, role: Role) =>
      api.patch<{ member: unknown }>(`/api/v2/admin/organizations/${org}/members/${userId}`, {
        role,
      }),
    remove: (userId: number) =>
      api.delete<{ deleted: boolean }>(`/api/v2/admin/organizations/${org}/members/${userId}`),
  }),
  invitations: (org: string) => ({
    list: () =>
      api.get<{ invitations: Invitation[] }>(`/api/v2/admin/organizations/${org}/invitations`),
    create: (email_address: string, role: Role) =>
      api.post<{ invitation: Invitation }>(`/api/v2/admin/organizations/${org}/invitations`, {
        email_address,
        role,
      }),
    revoke: (id: number) =>
      api.delete<{ deleted: boolean }>(`/api/v2/admin/organizations/${org}/invitations/${id}`),
  }),
  // Tenant SSO configuration (admin+): login-routing email domains and
  // the organization's OIDC/SAML/social connections.
  orgSso: (org: string) => {
    const base = `/api/v2/admin/organizations/${org}/sso`
    return {
      domains: {
        list: () => api.get<{ domains: OrgSsoDomain[] }>(`${base}/domains`),
        create: (domain: string) =>
          api.post<{ domain: OrgSsoDomain }>(`${base}/domains`, { domain }),
        verify: (id: number) =>
          api.post<{ domain: OrgSsoDomain }>(`${base}/domains/${id}/verify`, {}),
        delete: (id: number) => api.delete<{ deleted: boolean }>(`${base}/domains/${id}`),
      },
      connections: {
        list: () => api.get<{ connections: OrgSsoConnection[] }>(`${base}/connections`),
        create: (fields: {
          kind: OrgSsoKind
          name: string
          config: Record<string, string>
          default_role?: Role
          auto_provision?: boolean
          enabled?: boolean
        }) => api.post<{ connection: OrgSsoConnection }>(`${base}/connections`, fields),
        update: (
          id: number,
          fields: Partial<{
            name: string
            enabled: boolean
            config: Record<string, string>
            default_role: Role
            auto_provision: boolean
          }>,
        ) =>
          api.patch<{ connection: OrgSsoConnection }>(`${base}/connections/${id}`, fields),
        delete: (id: number) =>
          api.delete<{ deleted: boolean }>(`${base}/connections/${id}`),
      },
    }
  },
  servers: (org: string) => ({
    list: () =>
      api.get<{ servers: Server[]; pagination: Pagination }>(
        `/api/v2/admin/organizations/${org}/servers?per_page=100`,
      ),
    // Per-server 30-day message counters for the dashboard servers table.
    stats: () =>
      api.get<{ stats: ServerStat[] }>(
        `/api/v2/admin/organizations/${org}/servers/stats`,
      ),
    // Full windowed message statistics for one server (admin/Bearer, no
    // server API key) — powers the detailed stats on the server dashboard.
    statsWindow: (server: string, from: Date, to: Date) =>
      api.get<{ stats: WindowStats }>(
        `/api/v2/admin/organizations/${org}/servers/${server}/stats?from=${encodeURIComponent(
          from.toISOString(),
        )}&to=${encodeURIComponent(to.toISOString())}`,
      ),
    get: (server: string) =>
      api.get<{ server: Server }>(`/api/v2/admin/organizations/${org}/servers/${server}`),
    create: (name: string, mode: string) =>
      api.post<{ server: Server }>(`/api/v2/admin/organizations/${org}/servers`, { name, mode }),
    update: (server: string, fields: Partial<Server>) =>
      api.patch<{ server: Server }>(
        `/api/v2/admin/organizations/${org}/servers/${server}`,
        fields,
      ),
    delete: (server: string) =>
      api.delete<{ deleted: boolean }>(`/api/v2/admin/organizations/${org}/servers/${server}`),
    suspend: (server: string, reason?: string) =>
      api.post<{ server: Server }>(
        `/api/v2/admin/organizations/${org}/servers/${server}/suspend`,
        reason ? { reason } : {},
      ),
    unsuspend: (server: string) =>
      api.post<{ server: Server }>(
        `/api/v2/admin/organizations/${org}/servers/${server}/unsuspend`,
      ),
    setIpPool: (server: string, ip_pool_id: number | null) =>
      api.post<{ server: Server }>(
        `/api/v2/admin/organizations/${org}/servers/${server}/ip_pool`,
        { ip_pool_id },
      ),
  }),
  domains: (org: string, server: string) => {
    const base = `/api/v2/admin/organizations/${org}/servers/${server}/domains`
    return {
      list: () => api.get<{ domains: Domain[]; pagination: Pagination }>(`${base}?per_page=100`),
      create: (name: string) => api.post<{ domain: Domain }>(base, { name }),
      verify: (name: string) => api.post<{ domain: Domain }>(`${base}/${name}/verify`),
      health: (name: string) => api.get<{ health: DomainHealth }>(`${base}/${name}/health`),
      delete: (name: string) => api.delete<{ deleted: boolean }>(`${base}/${name}`),
    }
  },
  credentials: (org: string, server: string) => {
    const base = `/api/v2/admin/organizations/${org}/servers/${server}/credentials`
    return {
      list: () =>
        api.get<{ credentials: Credential[]; pagination: Pagination }>(`${base}?per_page=100`),
      create: (fields: { type: string; name: string; key?: string }) =>
        api.post<{ credential: Credential }>(base, fields),
      update: (id: number, fields: { name?: string; hold?: boolean }) =>
        api.patch<{ credential: Credential }>(`${base}/${id}`, fields),
      delete: (id: number) => api.delete<{ deleted: boolean }>(`${base}/${id}`),
    }
  },
  routes: (org: string, server: string) => {
    const base = `/api/v2/admin/organizations/${org}/servers/${server}/routes`
    return {
      list: () => api.get<{ routes: Route[]; pagination: Pagination }>(`${base}?per_page=100`),
      create: (fields: {
        name: string
        mode: string
        domain_id?: number
        endpoint_url?: string
      }) => api.post<{ route: Route }>(base, fields),
      update: (id: number, fields: Record<string, unknown>) =>
        api.patch<{ route: Route }>(`${base}/${id}`, fields),
      delete: (id: number) => api.delete<{ deleted: boolean }>(`${base}/${id}`),
    }
  },
  webhooks: (org: string, server: string) => {
    const base = `/api/v2/admin/organizations/${org}/servers/${server}/webhooks`
    return {
      list: () =>
        api.get<{ webhooks: Webhook[]; pagination: Pagination }>(`${base}?per_page=100`),
      create: (fields: {
        name: string
        url: string
        all_events?: boolean
        sign?: boolean
        events?: string[]
        headers?: Record<string, string>
      }) => api.post<{ webhook: Webhook }>(base, fields),
      update: (
        id: number,
        fields: {
          name?: string
          url?: string
          sign?: boolean
          enabled?: boolean
          events?: string[]
          headers?: Record<string, string>
        },
      ) => api.patch<{ webhook: Webhook }>(`${base}/${id}`, fields),
      enable: (id: number) => api.post<{ webhook: Webhook }>(`${base}/${id}/enable`),
      disable: (id: number) => api.post<{ webhook: Webhook }>(`${base}/${id}/disable`),
      test: (id: number, event: string) =>
        api.post<{ result: WebhookTestResult }>(`${base}/${id}/test`, { event }),
      delete: (id: number) => api.delete<{ deleted: boolean }>(`${base}/${id}`),
    }
  },
  senderAddresses: (org: string, server: string) => {
    const base = `/api/v2/admin/organizations/${org}/servers/${server}/sender_addresses`
    return {
      list: () =>
        api.get<{ sender_addresses: SenderAddress[]; pagination: Pagination }>(
          `${base}?per_page=100`,
        ),
      // When the instance cannot email the confirmation link, the response
      // carries a one-time `verification_token` to relay manually.
      create: (email: string) =>
        api.post<{ sender_address: SenderAddress; verification_token?: string }>(base, {
          email,
        }),
      delete: (id: number) => api.delete<{ deleted: boolean }>(`${base}/${id}`),
    }
  },
  templates: (org: string, server: string) => {
    const base = `/api/v2/admin/organizations/${org}/servers/${server}/templates`
    return {
      copyTo: (permalink: string, target_server: string, overwrite = false) =>
        api.post<{ template: Template; overwritten: boolean }>(
          `${base}/${permalink}/copy_to`,
          { target_server, ...(overwrite ? { overwrite } : {}) },
        ),
    }
  },
  suppressions: (org: string, server: string) => {
    const base = `/api/v2/admin/organizations/${org}/servers/${server}/suppressions`
    return {
      list: () =>
        api.get<{
          suppressions: Suppression[]
          streams: SuppressionStreamRef[]
          pagination: Pagination
        }>(`${base}?per_page=100`),
      create: (fields: { type: string; address: string; reason?: string }) =>
        api.post<{ suppression: Suppression }>(base, fields),
      delete: (address: string) =>
        api.delete<{ deleted: boolean }>(`${base}/${encodeURIComponent(address)}`),
    }
  },
  users: {
    list: () => api.get<{ users: User[]; pagination: Pagination }>("/api/v2/admin/users?per_page=100"),
    create: (fields: {
      email_address: string
      first_name?: string
      last_name?: string
      admin?: boolean
      password?: string
    }) => api.post<{ user: User }>("/api/v2/admin/users", fields),
    update: (id: number, fields: Record<string, unknown>) =>
      api.patch<{ user: User }>(`/api/v2/admin/users/${id}`, fields),
    delete: (id: number) => api.delete<{ deleted: boolean }>(`/api/v2/admin/users/${id}`),
  },
  ipPools: {
    list: () =>
      api.get<{ ip_pools: IpPool[]; pagination: Pagination }>("/api/v2/admin/ip_pools?per_page=100"),
    create: (name: string, isDefault: boolean) =>
      api.post<{ ip_pool: IpPool }>("/api/v2/admin/ip_pools", { name, default: isDefault }),
    delete: (id: number) => api.delete<{ deleted: boolean }>(`/api/v2/admin/ip_pools/${id}`),
    addresses: (poolId: number) => ({
      list: () =>
        api.get<{ ip_addresses: IpAddress[]; pagination: Pagination }>(
          `/api/v2/admin/ip_pools/${poolId}/ip_addresses?per_page=100`,
        ),
      create: (fields: { ipv4: string; ipv6?: string; hostname: string; priority?: number }) =>
        api.post<{ ip_address: IpAddress }>(
          `/api/v2/admin/ip_pools/${poolId}/ip_addresses`,
          fields,
        ),
      delete: (id: number) =>
        api.delete<{ deleted: boolean }>(`/api/v2/admin/ip_pools/${poolId}/ip_addresses/${id}`),
    }),
  },
  adminApiKeys: {
    list: () =>
      api.get<{ admin_api_keys: AdminApiKey[]; pagination: Pagination }>(
        "/api/v2/admin/admin_api_keys?per_page=100",
      ),
    create: (name: string) =>
      api.post<{ admin_api_key: AdminApiKey }>("/api/v2/admin/admin_api_keys", { name }),
    delete: (id: number) =>
      api.delete<{ deleted: boolean }>(`/api/v2/admin/admin_api_keys/${id}`),
  },
  authEvents: {
    list: (limit = 200) =>
      api.get<{ auth_events: AuthEvent[] }>(`/api/v2/admin/auth_events?limit=${limit}`),
  },
}

// ---------------------------------------------- server (messaging) API

/// The per-server messaging API authenticates with an API credential of
/// the server (`X-Server-API-Key`), not with the user session.
export function serverApi(key: string) {
  const h = { "X-Server-API-Key": key }
  return {
    send: (fields: {
      from: string
      to: string[]
      cc?: string[]
      bcc?: string[]
      subject?: string
      html_body?: string
      text_body?: string
      stream?: string
      tag?: string
    }) =>
      api.post<{ message_id: number; recipients: { rcpt_to: string; status: string; token: string }[] }>(
        "/api/v2/server/messages",
        fields,
        h,
      ),
    sendWithTemplate: (fields: {
      from: string
      to: string[]
      template: string
      template_model?: unknown
      stream?: string
    }) =>
      api.post<{ message_id: number; recipients: unknown[] }>(
        "/api/v2/server/messages/with_template",
        fields,
        h,
      ),
    info: () => api.get<{ server: Server }>("/api/v2/server/", h),
    messages: (params = "") =>
      api.get<{ messages: Message[]; pagination: Pagination }>(
        `/api/v2/server/messages${params}`,
        h,
      ),
    message: (id: number) => api.get<{ message: Message }>(`/api/v2/server/messages/${id}`, h),
    deliveries: (id: number) =>
      api.get<{ deliveries: Delivery[] }>(`/api/v2/server/messages/${id}/deliveries`, h),
    raw: (id: number) => api.get<{ raw: string }>(`/api/v2/server/messages/${id}/raw`, h),
    share: (id: number, expires_in_hours: number) =>
      api.post<MessageShare>(`/api/v2/server/messages/${id}/share`, { expires_in_hours }, h),
    insights: (id: number) =>
      api.get<MessageInsights>(`/api/v2/server/messages/${id}/insights`, h),
    // inbound queue management — the endpoint returns the rows under the
    // "inbound" key (not "messages"), matching the server API contract.
    inbound: (params = "") =>
      api.get<{ inbound: Message[]; pagination: Pagination }>(
        `/api/v2/server/inbound${params}`,
        h,
      ),
    inboundRetry: (id: number) => api.post<unknown>(`/api/v2/server/inbound/${id}/retry`, {}, h),
    inboundBypass: (id: number) =>
      api.post<unknown>(`/api/v2/server/inbound/${id}/bypass`, {}, h),
    stats: () => api.get<{ stats: Stats }>("/api/v2/server/stats", h),
    deliveryStats: () => api.get<{ delivery_stats: unknown }>("/api/v2/server/stats/deliveries", h),
    bounces: () =>
      api.get<{ bounces: Message[]; pagination?: Pagination }>("/api/v2/server/bounces", h),
    streams: {
      list: () => api.get<{ streams: Stream[] }>("/api/v2/server/streams", h),
      create: (fields: { name: string; stream_type?: string }) =>
        api.post<{ stream: Stream }>("/api/v2/server/streams", fields, h),
      update: (permalink: string, fields: Record<string, unknown>) =>
        api.patch<{ stream: Stream }>(`/api/v2/server/streams/${permalink}`, fields, h),
      archive: (permalink: string) =>
        api.post<unknown>(`/api/v2/server/streams/${permalink}/archive`, {}, h),
      // Opt-in subscribers of a (broadcast) stream: broadcast sends are only
      // allowed to addresses with an active subscription here.
      subscribers: (permalink: string) => ({
        list: () =>
          api.get<{ subscribers: Subscription[] }>(
            `/api/v2/server/streams/${permalink}/subscribers`,
            h,
          ),
        add: (address: string, status?: string) =>
          api.post<{ subscriber: Subscription }>(
            `/api/v2/server/streams/${permalink}/subscribers`,
            { address, ...(status ? { status } : {}) },
            h,
          ),
        remove: (address: string) =>
          api.delete<{ deleted: boolean }>(
            `/api/v2/server/streams/${permalink}/subscribers/${encodeURIComponent(address)}`,
            h,
          ),
      }),
    },
    dmarc: {
      summary: (params = "") =>
        api.get<{ summary: DmarcSummary }>(`/api/v2/server/dmarc/summary${params}`, h),
      reports: (params = "") =>
        api.get<{ reports: DmarcReport[]; pagination: Pagination }>(
          `/api/v2/server/dmarc/reports${params}`,
          h,
        ),
      report: (id: number) =>
        api.get<{ report: DmarcReport; records: DmarcRecord[] }>(
          `/api/v2/server/dmarc/reports/${id}`,
          h,
        ),
    },
    templates: {
      list: () => api.get<{ templates: Template[] }>("/api/v2/server/templates", h),
      create: (fields: {
        name: string
        subject?: string
        html_body?: string
        text_body?: string
        layout?: string
      }) => api.post<{ template: Template }>("/api/v2/server/templates", fields, h),
      update: (permalink: string, fields: Record<string, unknown>) =>
        api.patch<{ template: Template }>(`/api/v2/server/templates/${permalink}`, fields, h),
      archive: (permalink: string) =>
        api.post<unknown>(`/api/v2/server/templates/${permalink}/archive`, {}, h),
      render: (permalink: string, model: unknown) =>
        api.post<{ rendered: { subject: string | null; html_body: string | null; text_body: string | null } }>(
          `/api/v2/server/templates/${permalink}/render`,
          { template_model: model },
          h,
        ),
    },
    layouts: {
      list: () => api.get<{ layouts: Layout[] }>("/api/v2/server/layouts", h),
      create: (fields: {
        name: string
        html_wrapper: string
        text_wrapper?: string
      }) => api.post<{ layout: Layout }>("/api/v2/server/layouts", fields, h),
      update: (permalink: string, fields: Record<string, unknown>) =>
        api.patch<{ layout: Layout }>(`/api/v2/server/layouts/${permalink}`, fields, h),
      delete: (permalink: string) =>
        api.delete<{ deleted: boolean }>(`/api/v2/server/layouts/${permalink}`, h),
    },
  }
}
