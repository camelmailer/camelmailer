"use client"

import { OrgShell } from "@/views/org/OrgHome"
import { useOrgParams } from "@/lib/params"

export default function Layout({ children }: { children: React.ReactNode }) {
  const { org } = useOrgParams()
  return <OrgShell org={org}>{children}</OrgShell>
}
