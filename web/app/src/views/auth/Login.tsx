"use client"

import { useEffect, useState } from "react"
import Link from "next/link"
import { useRouter } from "next/navigation"
import { Alert, AlertDescription } from "@/components/ui/alert"
import { AuthShell } from "@/components/auth-shell"
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
    <AuthShell title="Sign in" description="Welcome back to CamelMailer">
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
            {orgConnections.map((connection) => (
              <Button
                key={connection.id}
                type="button"
                variant="outline"
                onClick={() => (window.location.href = connection.start_url)}
              >
                Continue with {connection.name}
              </Button>
            ))}
            {features?.webauthn && (
              <Button
                type="button"
                variant="outline"
                onClick={passkeyLogin}
                disabled={busy}
              >
                Sign in with passkey
              </Button>
            )}
            {ssoUrl && (
              <Button
                type="button"
                variant="outline"
                onClick={() => (window.location.href = ssoUrl)}
              >
                Continue with SSO
              </Button>
            )}
            {ssoProviders.map((provider) => (
              <Button
                key={provider.id}
                type="button"
                variant="outline"
                onClick={() => (window.location.href = ssoStartUrl(provider.id))}
              >
                Continue with {provider.name}
              </Button>
            ))}
            {features?.registration && (
              <p className="text-center text-sm text-muted-foreground">
                Don&apos;t have an account?{" "}
                <Link href="/register" className="hover:underline">
                  Create account
                </Link>
              </p>
            )}
            {saml && (
              <Button
                type="button"
                variant="outline"
                onClick={() => (window.location.href = saml.url)}
              >
                Sign in with {saml.name}
              </Button>
            )}
          </form>
    </AuthShell>
  )
}
