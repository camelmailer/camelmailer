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
}

export type Role = "viewer" | "member" | "admin" | "owner"

export type Membership = {
  role: Role
  created_at: string
  user: User
}

export type MeResponse = {
  user: User
  memberships: { role: Role; organization: Organization }[]
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
}

export type Domain = { id: number; uuid: string; name: string; verified: boolean }

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
}

export type Suppression = {
  id: number
  type: string
  address: string
  reason: string | null
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
}

export type PasskeyCredential = {
  id: number
  name: string
  created_at: string
  last_used_at: string | null
}

export type Pagination = { page: number; per_page: number; total: number; total_pages: number }

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
}

// -------------------------------------------------------------- auth API

export const authApi = {
  login: (email_address: string, password: string, totp_code?: string) =>
    api.post<{ session_token: string; expires_at: string; user: User }>(
      "/api/v2/auth/login",
      { email_address, password, ...(totp_code ? { totp_code } : {}) },
    ),
  // 403 RegistrationDisabled unless auth.allow_registration is on.
  register: (fields: {
    email_address: string
    first_name: string
    last_name: string
    password: string
  }) =>
    api.post<{ session_token: string; expires_at: string; user: User }>(
      "/api/v2/auth/register",
      fields,
    ),
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
  // 404 SSODisabled when OIDC is off — used to decide whether to render
  // the SSO button.
  oidcStartUrl: () =>
    api.get<{ authorization_url: string }>("/api/v2/auth/oidc/start", {
      Accept: "application/json",
    }),
  // Which optional login features this instance exposes (passkeys,
  // self-registration, OIDC) — unauthenticated, drives the login page.
  features: () => api.get<Features>("/api/v2/auth/features"),
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

// ------------------------------------------------------------- admin API

export const adminApi = {
  organizations: {
    list: () =>
      api.get<{ organizations: Organization[]; pagination: Pagination }>(
        "/api/v2/admin/organizations?per_page=100",
      ),
    create: (name: string) =>
      api.post<{ organization: Organization }>("/api/v2/admin/organizations", { name }),
    delete: (permalink: string) =>
      api.delete<{ deleted: boolean }>(`/api/v2/admin/organizations/${permalink}`),
  },
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
  servers: (org: string) => ({
    list: () =>
      api.get<{ servers: Server[]; pagination: Pagination }>(
        `/api/v2/admin/organizations/${org}/servers?per_page=100`,
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
      create: (fields: { name: string; url: string; all_events?: boolean; sign?: boolean }) =>
        api.post<{ webhook: Webhook }>(base, fields),
      enable: (id: number) => api.post<{ webhook: Webhook }>(`${base}/${id}/enable`),
      disable: (id: number) => api.post<{ webhook: Webhook }>(`${base}/${id}/disable`),
      delete: (id: number) => api.delete<{ deleted: boolean }>(`${base}/${id}`),
    }
  },
  suppressions: (org: string, server: string) => {
    const base = `/api/v2/admin/organizations/${org}/servers/${server}/suppressions`
    return {
      list: () =>
        api.get<{ suppressions: Suppression[]; pagination: Pagination }>(`${base}?per_page=100`),
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
    // inbound queue management
    inbound: (params = "") =>
      api.get<{ messages: Message[]; pagination: Pagination }>(
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
    },
    templates: {
      list: () => api.get<{ templates: Template[] }>("/api/v2/server/templates", h),
      create: (fields: {
        name: string
        subject?: string
        html_body?: string
        text_body?: string
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
  }
}
