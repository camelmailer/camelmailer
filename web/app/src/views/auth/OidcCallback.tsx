"use client"

import { useEffect, useState } from "react"
import { useRouter } from "next/navigation"
import { Alert, AlertDescription } from "@/components/ui/alert"
import { useAuth } from "@/lib/auth"

/// Landing page for the backend's SSO redirect:
/// `{frontend_url}/auth/callback#session_token=…`
export default function OidcCallback() {
  const { adopt } = useAuth()
  const router = useRouter()
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    const fragment = new URLSearchParams(window.location.hash.replace(/^#/, ""))
    const token = fragment.get("session_token")
    if (!token) {
      setError("No session token in the callback URL.")
      return
    }
    // Drop the token from the address bar before adopting it.
    window.history.replaceState(null, "", window.location.pathname)
    adopt(token)
      .then(() => router.replace("/dashboard"))
      .catch(() => setError("The SSO session could not be established."))
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  return (
    <div className="flex min-h-svh items-center justify-center p-4">
      {error ? (
        <Alert variant="destructive" className="max-w-md">
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      ) : (
        <p className="text-muted-foreground">Completing sign-in…</p>
      )}
    </div>
  )
}
