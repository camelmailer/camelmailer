"use client"

import { MessagingShell } from "@/views/server/Messaging"
import { useOrgParams } from "@/lib/params"

export default function Layout({ children }: { children: React.ReactNode }) {
  const { org, server } = useOrgParams()
  return (
    <MessagingShell org={org} server={server}>
      {children}
    </MessagingShell>
  )
}
