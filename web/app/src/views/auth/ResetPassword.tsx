"use client"

import { useState } from "react"
import Link from "next/link"
import { useSearchParams } from "next/navigation"
import { Alert, AlertDescription } from "@/components/ui/alert"
import { AuthShell } from "@/components/auth-shell"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { ApiError, authApi } from "@/lib/api"

/// `/forgot-password` (request) and `/reset-password?token=…` (complete)
/// share this component: the presence of a token decides the mode.
export default function ResetPassword() {
  const params = useSearchParams()
  const token = params.get("token")

  const [email, setEmail] = useState("")
  const [password, setPassword] = useState("")
  const [message, setMessage] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  async function submit(event: React.FormEvent) {
    event.preventDefault()
    setBusy(true)
    setError(null)
    setMessage(null)
    try {
      if (token) {
        await authApi.completeReset(token, password)
        setMessage("Password updated. You can sign in now.")
      } else {
        await authApi.requestReset(email)
        setMessage(
          "If an account exists for this address, a reset link has been issued. " +
            "Ask your administrator for the link if you don't receive it.",
        )
      }
    } catch (err) {
      setError(err instanceof ApiError ? err.message : "Request failed.")
    } finally {
      setBusy(false)
    }
  }

  return (
    <AuthShell
      title={token ? "Choose a new password" : "Reset your password"}
      description={
        token
          ? "Enter the new password for your account."
          : "We'll issue a single-use reset link for your account."
      }
    >
          <form onSubmit={submit} className="grid gap-4">
            {token ? (
              <div className="grid gap-2">
                <Label htmlFor="password">New password</Label>
                <Input
                  id="password"
                  type="password"
                  autoComplete="new-password"
                  minLength={8}
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                  required
                />
              </div>
            ) : (
              <div className="grid gap-2">
                <Label htmlFor="email">Email</Label>
                <Input
                  id="email"
                  type="email"
                  value={email}
                  onChange={(e) => setEmail(e.target.value)}
                  required
                />
              </div>
            )}
            {message && (
              <Alert>
                <AlertDescription>{message}</AlertDescription>
              </Alert>
            )}
            {error && (
              <Alert variant="destructive">
                <AlertDescription>{error}</AlertDescription>
              </Alert>
            )}
            <Button type="submit" disabled={busy}>
              {token ? "Set password" : "Request reset"}
            </Button>
            <Link href="/login" className="text-center text-xs text-muted-foreground hover:underline">
              Back to login
            </Link>
          </form>
    </AuthShell>
  )
}
