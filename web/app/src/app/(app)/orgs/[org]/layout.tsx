"use client"

import { OrgTwoFactorGate } from "@/views/org/OrgHome"
import { useOrgParams } from "@/lib/params"

export default function Layout({ children }: { children: React.ReactNode }) {
  const { org } = useOrgParams()
  return <OrgTwoFactorGate org={org}>{children}</OrgTwoFactorGate>
}
