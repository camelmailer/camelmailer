"use client"

import { useEffect, useState } from "react"
import Link from "next/link"
import { useRouter } from "next/navigation"
import { Fingerprint, KeyRound } from "lucide-react"
import { Alert, AlertDescription } from "@/components/ui/alert"
import { AuthLegal, AuthShell } from "@/components/auth-shell"
import { AuthDivider, ProviderButton, providerIcon } from "@/components/auth-social"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import {
  ApiError,
  authApi,
  ssoStartUrl,
  type DiscoveredSsoConnection,
  type Features,
  type SsoProviderInfo,
} from "@/lib/api"
import { useAuth } from "@/lib/auth"
import { getPasskeyAssertion, webAuthnSupported, type RequestOptionsJSON } from "@/lib/webauthn"

export default function Login() {
  const { adopt, token, me, loading } = useAuth()
  const router = useRouter()

  // Already signed in? Don't ask again — go to the dashboard. Covers
  // returning to /login (or the root gate) with a live session.
  useEffect(() => {
    if (!loading && token && me) router.replace("/dashboard")
  }, [loading, token, me, router])

  const [email, setEmail] = useState("")
  const [password, setPassword] = useState("")
  const [totpCode, setTotpCode] = useState("")
  const [totpRequired, setTotpRequired] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const [ssoUrl, setSsoUrl] = useState<string | null>(null)
  const [ssoProviders, setSsoProviders] = useState<SsoProviderInfo[]>([])
  const [features, setFeatures] = useState<Features | null>(null)
  const [saml, setSaml] = useState<{ url: string; name: string } | null>(null)
  const [orgConnections, setOrgConnections] = useState<DiscoveredSsoConnection[]>([])

  // Tenant SSO discovery: when the email's domain is verified by an
  // organization, offer its connections. Silent on failure — the page
  // simply keeps the password sign-in.
  async function discoverOrgSso(address: string) {
    if (!address.includes("@")) return
    try {
      const { connections } = await authApi.orgSsoDiscover(address)
      setOrgConnections(connections)
    } catch {
      setOrgConnections([])
    }
  }

  // /features says which optional sign-in paths this instance exposes
  // (passkeys, self-registration, SSO). The OIDC and SAML start URLs are
  // fetched only when the feature flags report them enabled, so a plain
  // instance never probes endpoints that would 404.
  useEffect(() => {
    authApi
      .features()
      .then((loaded) => {
        setFeatures(loaded)
        setSsoProviders(loaded.sso ?? [])
        if (loaded.oidc.enabled) {
          authApi
            .oidcStartUrl()
            .then((data) => setSsoUrl(data.authorization_url))
            .catch(() => setSsoUrl(null))
        }
        if (loaded.saml.enabled) {
          authApi
            .samlStartUrl()
            .then((data) =>
              setSaml({ url: data.authorization_url, name: data.name || "SAML" }),
            )
            .catch(() => setSaml(null))
        }
      })
      .catch(() => setFeatures(null))
  }, [])

  async function passkeyLogin() {
    if (!email) {
      setError("Enter your email address first, then use your passkey.")
      return
    }
    if (!webAuthnSupported()) {
      setError("This browser does not support passkeys.")
      return
    }
    setBusy(true)
    setError(null)
    try {
      const options = await authApi.webauthnLoginStart(email)
      const assertion = await getPasskeyAssertion(options as unknown as RequestOptionsJSON)
      const result = await authApi.webauthnLoginFinish(assertion)
      await adopt(result.session_token)
      router.push("/dashboard")
    } catch (err) {
      if (err instanceof ApiError) {
        if (err.code === "AccountLocked") {
          setError("Account temporarily locked after repeated failures. Try again later.")
        } else {
          setError("The passkey sign-in did not succeed.")
        }
      } else {
        setError("The passkey sign-in did not succeed.")
      }
    } finally {
      setBusy(false)
    }
  }

  async function submit(event: React.FormEvent) {
    event.preventDefault()
    setBusy(true)
    setError(null)
    try {
      const result = await authApi.login(email, password, totpCode || undefined)
      await adopt(result.session_token)
      router.push("/dashboard")
    } catch (err) {
      if (err instanceof ApiError) {
        if (err.code === "TOTPRequired") {
          setTotpRequired(true)
          setError(null)
        } else if (err.code === "AccountLocked") {
          setError("Account temporarily locked after repeated failures. Try again later.")
        } else if (err.code === "InvalidTOTPCode") {
          setTotpRequired(true)
          setError("The two-factor code is incorrect.")
        } else {
          setError(err.message)
        }
      } else {
        setError("Could not reach the server.")
      }
    } finally {
      setBusy(false)
    }
  }

  return (
    <AuthShell
      title="Welcome back"
      description={
        features?.registration ? (
          <>
            Sign in or{" "}
            <Link
              href="/register"
              className="font-medium text-foreground underline-offset-4 hover:underline"
            >
              create account
            </Link>
          </>
        ) : (
          "Sign in to your account"
        )
      }
      footer={<AuthLegal legal={features?.legal} />}
    >
          <form onSubmit={submit} className="grid gap-4">
            <div className="grid gap-2">
              <Label htmlFor="email">Email</Label>
              <Input
                id="email"
                type="email"
                autoComplete="username"
                value={email}
                onChange={(e) => {
                  setEmail(e.target.value)
                  setOrgConnections([])
                }}
                onBlur={() => discoverOrgSso(email)}
                required
                autoFocus
              />
            </div>
            <div className="grid gap-2">
              <div className="flex items-center justify-between">
                <Label htmlFor="password">Password</Label>
                <Link
                  href="/forgot-password"
                  className="text-xs text-muted-foreground hover:underline"
                >
                  Forgot password?
                </Link>
              </div>
              <Input
                id="password"
                type="password"
                autoComplete="current-password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                required
              />
            </div>
            {totpRequired && (
              <div className="grid gap-2">
                <Label htmlFor="totp">Two-factor code</Label>
                <Input
                  id="totp"
                  inputMode="numeric"
                  pattern="[0-9]{6}"
                  placeholder="123456"
                  value={totpCode}
                  onChange={(e) => setTotpCode(e.target.value)}
                  autoFocus
                />
              </div>
            )}
            {error && (
              <Alert variant="destructive">
                <AlertDescription>{error}</AlertDescription>
              </Alert>
            )}
            <Button type="submit" disabled={busy}>
              {busy ? "Signing in…" : "Sign in"}
            </Button>
            {(features?.webauthn ||
              ssoUrl ||
              saml ||
              ssoProviders.length > 0 ||
              orgConnections.length > 0) && <AuthDivider />}
            {features?.webauthn && (
              <ProviderButton
                icon={<Fingerprint />}
                label="Sign in with a passkey"
                onClick={passkeyLogin}
                disabled={busy}
              />
            )}
            {ssoProviders.map((provider) => (
              <ProviderButton
                key={provider.id}
                icon={providerIcon(provider)}
                label={`Continue with ${provider.name}`}
                onClick={() => (window.location.href = ssoStartUrl(provider.id))}
              />
            ))}
            {orgConnections.map((connection) => (
              <ProviderButton
                key={connection.id}
                icon={<KeyRound />}
                label={`Continue with ${connection.name}`}
                onClick={() => (window.location.href = connection.start_url)}
              />
            ))}
            {ssoUrl && (
              <ProviderButton
                icon={<KeyRound />}
                label="Continue with SSO"
                onClick={() => (window.location.href = ssoUrl)}
              />
            )}
            {saml && (
              <ProviderButton
                icon={<KeyRound />}
                label={`Sign in with ${saml.name}`}
                onClick={() => (window.location.href = saml.url)}
              />
            )}
          </form>
    </AuthShell>
  )
}
