"use client"

import { useEffect, useState } from "react"
import { useRouter, useSearchParams } from "next/navigation"
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

type Preview = {
  email_address: string
  role: string
  organization: { name: string; permalink: string } | null
  user_exists: boolean
}

export default function AcceptInvitation() {
  const params = useSearchParams()
  const token = params.get("token") ?? ""
  const router = useRouter()
  const { adopt } = useAuth()

  const [preview, setPreview] = useState<Preview | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [firstName, setFirstName] = useState("")
  const [lastName, setLastName] = useState("")
  const [password, setPassword] = useState("")
  const [busy, setBusy] = useState(false)
  const [done, setDone] = useState(false)

  useEffect(() => {
    if (!token) {
      setError("This invitation link is missing its token.")
      return
    }
    authApi
      .invitationPreview(token)
      .then((data) => setPreview(data.invitation as Preview))
      .catch((err) =>
        setError(err instanceof ApiError ? err.message : "Could not load the invitation."),
      )
  }, [token])

  async function submit(event: React.FormEvent) {
    event.preventDefault()
    setBusy(true)
    setError(null)
    try {
      const result = await authApi.invitationAccept({
        token,
        ...(preview?.user_exists
          ? {}
          : { first_name: firstName, last_name: lastName, password }),
      })
      if (result.session_token) {
        await adopt(result.session_token)
        router.replace("/dashboard")
      } else {
        setDone(true)
      }
    } catch (err) {
      setError(err instanceof ApiError ? err.message : "Accepting the invitation failed.")
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="flex min-h-svh items-center justify-center bg-muted/40 p-4">
      <Card className="w-full max-w-sm">
        <CardHeader>
          <CardTitle className="text-xl">Join {preview?.organization?.name ?? "…"}</CardTitle>
          <CardDescription>
            {preview
              ? `You've been invited as ${preview.role} (${preview.email_address}).`
              : "Loading invitation…"}
          </CardDescription>
        </CardHeader>
        <CardContent>
          {done ? (
            <Alert>
              <AlertDescription>
                Membership added. Sign in with your existing account to continue.{" "}
                <a className="underline" href="/login">
                  Go to login
                </a>
              </AlertDescription>
            </Alert>
          ) : (
            <form onSubmit={submit} className="grid gap-4">
              {preview && !preview.user_exists && (
                <>
                  <div className="grid grid-cols-2 gap-2">
                    <div className="grid gap-2">
                      <Label htmlFor="first">First name</Label>
                      <Input
                        id="first"
                        value={firstName}
                        onChange={(e) => setFirstName(e.target.value)}
                      />
                    </div>
                    <div className="grid gap-2">
                      <Label htmlFor="last">Last name</Label>
                      <Input
                        id="last"
                        value={lastName}
                        onChange={(e) => setLastName(e.target.value)}
                      />
                    </div>
                  </div>
                  <div className="grid gap-2">
                    <Label htmlFor="password">Choose a password</Label>
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
                </>
              )}
              {error && (
                <Alert variant="destructive">
                  <AlertDescription>{error}</AlertDescription>
                </Alert>
              )}
              <Button type="submit" disabled={busy || !preview}>
                {busy ? "Joining…" : "Accept invitation"}
              </Button>
            </form>
          )}
        </CardContent>
      </Card>
    </div>
  )
}
