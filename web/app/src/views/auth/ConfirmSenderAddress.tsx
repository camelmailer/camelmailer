"use client"

import { useState } from "react"
import Link from "next/link"
import { useSearchParams } from "next/navigation"
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

/// `/sender-addresses/confirm?token=…` — public confirmation page for a
/// sender address; the token from the email (or relayed by the operator)
/// is the secret.
export default function ConfirmSenderAddress() {
  const params = useSearchParams()
  const [token, setToken] = useState(params.get("token") ?? "")
  const [confirmed, setConfirmed] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  async function submit(event: React.FormEvent) {
    event.preventDefault()
    setBusy(true)
    setError(null)
    try {
      const { email_address } = await authApi.confirmSenderAddress(token)
      setConfirmed(email_address)
    } catch (err) {
      setError(
        err instanceof ApiError
          ? err.message
          : "Confirmation failed. Check the link and try again.",
      )
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="flex min-h-svh items-center justify-center bg-muted/40 p-4">
      <Card className="w-full max-w-sm">
        <CardHeader>
          <CardTitle className="text-xl">Confirm sender address</CardTitle>
          <CardDescription>
            Confirming authorizes a CamelMailer mail server to send email from
            your address.
          </CardDescription>
        </CardHeader>
        <CardContent>
          {confirmed ? (
            <Alert>
              <AlertDescription>
                <strong>{confirmed}</strong> is confirmed as a sender address.
                You can close this page.
              </AlertDescription>
            </Alert>
          ) : (
            <form onSubmit={submit} className="grid gap-4">
              <div className="grid gap-2">
                <Label htmlFor="token">Confirmation token</Label>
                <Input
                  id="token"
                  value={token}
                  onChange={(e) => setToken(e.target.value)}
                  required
                />
              </div>
              {error && (
                <Alert variant="destructive">
                  <AlertDescription>{error}</AlertDescription>
                </Alert>
              )}
              <Button type="submit" disabled={busy || !token.trim()}>
                Confirm address
              </Button>
              <Link
                href="/login"
                className="text-center text-xs text-muted-foreground hover:underline"
              >
                Go to login
              </Link>
            </form>
          )}
        </CardContent>
      </Card>
    </div>
  )
}
