"use client"

import OrgSso from "@/views/org/OrgSso"
import { useOrgParams } from "@/lib/params"

export default function Page() {
  const { org } = useOrgParams()
  return <OrgSso org={org} />
}
