"use client"

import { Domains } from "@/views/server/ResourceTabs"
import { useOrgParams } from "@/lib/params"

export default function Page() {
  const { org, server } = useOrgParams()
  return <Domains org={org} server={server} />
}
