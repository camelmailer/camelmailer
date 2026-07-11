"use client"

import { Servers } from "@/views/org/OrgHome"
import { useOrgParams } from "@/lib/params"

export default function Page() {
  const { org } = useOrgParams()
  return <Servers org={org} />
}
