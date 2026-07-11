"use client"

import { Members } from "@/views/org/OrgHome"
import { useOrgParams } from "@/lib/params"

export default function Page() {
  const { org } = useOrgParams()
  return <Members org={org} />
}
