"use client"

import { useEffect, useState } from "react"
import Link from "next/link"
import { useRouter } from "next/navigation"
import { Alert, AlertDescription } from "@/components/ui/alert"
import { Button } from "@/components/ui/button"
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { ApiError, authApi } from "@/lib/api"
import { useAuth } from "@/lib/auth"

export default function Login() {
  const { adopt } = useAuth()
  const router = useRouter()
  const [email, setEmail] = useState("")
  const [password, setPassword] = useState("")
  const [totpCode, setTotpCode] = useState("")
  const [totpRequired, setTotpRequired] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const [ssoUrl, setSsoUrl] = useState<string | null>(null)
  const [saml, setSaml] = useState<{ url: string; name: string } | null>(null)

  // The SSO buttons only render when OIDC/SAML are enabled on the
  // instance (the start endpoints 404 otherwise).
  useEffect(() => {
    authApi
      .oidcStartUrl()
      .then((data) => setSsoUrl(data.authorization_url))
      .catch(() => setSsoUrl(null))
    authApi
      .samlStartUrl()
      .then((data) =>
        setSaml({ url: data.authorization_url, name: data.name || "SAML" }),
      )
      .catch(() => setSaml(null))
  }, [])

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
    <div className="flex min-h-svh items-center justify-center bg-muted/40 p-4">
      <Card className="w-full max-w-sm">
        <CardHeader>
          <CardTitle className="flex items-center gap-2 text-xl">
            {/* eslint-disable-next-line @next/next/no-img-element */}
            <img src="/camelmailer-symbol.png" alt="" className="size-7" />
            CamelMailer
          </CardTitle>
          <CardDescription>Sign in to your account</CardDescription>
        </CardHeader>
        <CardContent>
          <form onSubmit={submit} className="grid gap-4">
            <div className="grid gap-2">
              <Label htmlFor="email">Email</Label>
              <Input
                id="email"
                type="email"
                autoComplete="username"
                value={email}
                onChange={(e) => setEmail(e.target.value)}
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
            {ssoUrl && (
              <Button
                type="button"
                variant="outline"
                onClick={() => (window.location.href = ssoUrl)}
              >
                Continue with SSO
              </Button>
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
            <p className="text-center text-sm text-muted-foreground">
              Don&apos;t have an account?{" "}
              <Link href="/register" className="hover:underline">
                Create account
              </Link>
            </p>
          </form>
        </CardContent>
      </Card>
    </div>
  )
}
