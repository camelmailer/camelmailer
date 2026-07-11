"use client"

import { ServerShell } from "@/views/server/ServerHome"
import { useOrgParams } from "@/lib/params"

export default function Layout({ children }: { children: React.ReactNode }) {
  const { org, server } = useOrgParams()
  return (
    <ServerShell org={org} server={server}>
      {children}
    </ServerShell>
  )
}
