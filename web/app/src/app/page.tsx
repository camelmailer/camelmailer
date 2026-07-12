"use client"

// This app is the dashboard only (the marketing site lives at
// camelmailer.com, a separate deployment). The root path is an auth gate:
// signed-in visitors go straight to the dashboard, everyone else to login.
// Auth lives client-side (Bearer token in storage), so this can't be a
// server redirect — the server never sees the session.

import { useEffect } from "react"
import { useRouter } from "next/navigation"
import { useAuth } from "@/lib/auth"

export default function Home() {
  const router = useRouter()
  const { token, me, loading } = useAuth()

  useEffect(() => {
    if (loading) return
    router.replace(token && me ? "/dashboard" : "/login")
  }, [loading, token, me, router])

  return null
}
