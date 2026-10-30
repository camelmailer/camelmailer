"use client"

import { useCallback, useEffect, useState } from "react"
import Link from "next/link"
import { useRouter } from "next/navigation"
import { Alert, AlertDescription } from "@/components/ui/alert"
import { AuthLegal, AuthShell } from "@/components/auth-shell"
import { AuthDivider, ProviderButton, providerIcon } from "@/components/auth-social"
import { Button } from "@/components/ui/button"
import { CapWidget } from "@/components/cap-widget"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import {
  ApiError,
  authApi,
  ssoStartUrl,
  type Features,
  type SsoProviderInfo,
} from "@/lib/api"
import { useAuth } from "@/lib/auth"

export default function Register() {
  const { adopt } = useAuth()
  const router = useRouter()
  const [email, setEmail] = useState("")
  const [firstName, setFirstName] = useState("")
  const [lastName, setLastName] = useState("")
  const [password, setPassword] = useState("")
  const [error, setError] = useState<string | null>(null)
  const [disabled, setDisabled] = useState(false)
  const [busy, setBusy] = useState(false)
  const [ssoProviders, setSsoProviders] = useState<SsoProviderInfo[]>([])
  const [features, setFeatures] = useState<Features | null>(null)
  const [captchaToken, setCaptchaToken] = useState<string | null>(null)
  // Bumped to remount the widget after a rejected token, so it asks again.
  const [captchaRound, setCaptchaRound] = useState(0)
  const onCaptchaToken = useCallback((token: string | null) => setCaptchaToken(token), [])
  const captchaEndpoint =
    features?.captcha?.provider === "cap" ? features.captcha.endpoint : null

  // Social sign-in provisions accounts automatically, so the same
  // providers double as sign-up buttons.
  useEffect(() => {
    authApi
      .features()
      .then((data) => {
        setFeatures(data)
        setSsoProviders(data.sso)
      })
      .catch(() => setSsoProviders([]))
  }, [])

  async function submit(event: React.FormEvent) {
    event.preventDefault()
    if (captchaEndpoint && !captchaToken) {
      setError("Please complete the captcha first.")
      return
    }
    setBusy(true)
    setError(null)
    try {
      const result = await authApi.register({
        email_address: email,
        first_name: firstName,
        last_name: lastName,
        password,
        ...(captchaToken ? { captcha_token: captchaToken } : {}),
      })
      await adopt(result.session_token)
      router.push("/dashboard")
    } catch (err) {
      if (err instanceof ApiError) {
        if (err.code === "RegistrationDisabled") {
          setDisabled(true)
          setError(null)
        } else if (err.code === "CaptchaInvalid") {
          setCaptchaToken(null)
          setCaptchaRound((round) => round + 1)
          setError(err.message)
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
      title="Create your account"
      description="Start sending transactional email in minutes"
      footer={<AuthLegal legal={features?.legal} />}
    >
          {disabled ? (
            <div className="grid gap-4">
              <Alert>
                <AlertDescription>
                  Registration is disabled on this instance. Ask an
                  administrator to invite you instead.
                </AlertDescription>
              </Alert>
              <Button asChild variant="outline">
                <Link href="/login">Back to sign in</Link>
              </Button>
            </div>
          ) : (
            <form onSubmit={submit} className="grid gap-4">
              <div className="grid gap-2">
                <Label htmlFor="email">Email</Label>
                <Input
                  id="email"
                  type="email"
                  autoComplete="email"
                  value={email}
                  onChange={(e) => setEmail(e.target.value)}
                  required
                  autoFocus
                />
              </div>
              <div className="grid grid-cols-2 gap-4">
                <div className="grid gap-2">
                  <Label htmlFor="first-name">First name</Label>
                  <Input
                    id="first-name"
                    autoComplete="given-name"
                    value={firstName}
                    onChange={(e) => setFirstName(e.target.value)}
                    required
                  />
                </div>
                <div className="grid gap-2">
                  <Label htmlFor="last-name">Last name</Label>
                  <Input
                    id="last-name"
                    autoComplete="family-name"
                    value={lastName}
                    onChange={(e) => setLastName(e.target.value)}
                    required
                  />
                </div>
              </div>
              <div className="grid gap-2">
                <Label htmlFor="password">Password</Label>
                <Input
                  id="password"
                  type="password"
                  autoComplete="new-password"
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                  required
                />
              </div>
              {captchaEndpoint && (
                <CapWidget
                  key={captchaRound}
                  endpoint={captchaEndpoint}
                  onToken={onCaptchaToken}
                />
              )}
              {error && (
                <Alert variant="destructive">
                  <AlertDescription>{error}</AlertDescription>
                </Alert>
              )}
              <Button type="submit" disabled={busy || (!!captchaEndpoint && !captchaToken)}>
                {busy ? "Creating account…" : "Create account"}
              </Button>
              {ssoProviders.length > 0 && <AuthDivider label="Or sign up with" />}
              {ssoProviders.map((provider) => (
                <ProviderButton
                  key={provider.id}
                  icon={providerIcon(provider)}
                  label={`Sign up with ${provider.name}`}
                  onClick={() => (window.location.href = ssoStartUrl(provider.id))}
                />
              ))}
              <p className="text-center text-sm text-muted-foreground">
                Already have an account?{" "}
                <Link href="/login" className="hover:underline">
                  Sign in
                </Link>
              </p>
            </form>
          )}
    </AuthShell>
  )
}
