"use client"

import { OrgSettings } from "@/views/org/OrgHome"
import { useOrgParams } from "@/lib/params"

export default function Page() {
  const { org } = useOrgParams()
  return <OrgSettings org={org} />
}
