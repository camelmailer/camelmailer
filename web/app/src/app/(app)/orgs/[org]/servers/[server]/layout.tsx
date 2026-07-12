"use client"

import { ServerShell } from "@/views/server/ServerHome"

export default function Layout({ children }: { children: React.ReactNode }) {
  return <ServerShell>{children}</ServerShell>
}
