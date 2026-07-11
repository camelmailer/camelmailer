"use client"

import { Suppressions } from "@/views/server/ResourceTabs"
import { useOrgParams } from "@/lib/params"

export default function Page() {
  const { org, server } = useOrgParams()
  return <Suppressions org={org} server={server} />
}
