"use client"

import { Invitations } from "@/views/org/OrgHome"
import { useOrgParams } from "@/lib/params"

export default function Page() {
  const { org } = useOrgParams()
  return <Invitations org={org} />
}
