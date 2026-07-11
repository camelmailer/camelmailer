"use client"

// Signed-in area: gate on the session, then wrap in the sidebar shell.

import { useRouter } from "next/navigation"
import { useEffect } from "react"
import AppShell from "@/views/AppShell"
import { useAuth } from "@/lib/auth"

export default function AppLayout({ children }: { children: React.ReactNode }) {
  const { token, loading } = useAuth()
  const router = useRouter()

  useEffect(() => {
    if (!loading && !token) router.replace("/login")
  }, [loading, token, router])

  if (loading || !token) {
    return <div className="p-8 text-sm text-muted-foreground">Loading…</div>
  }
  return <AppShell>{children}</AppShell>
}
