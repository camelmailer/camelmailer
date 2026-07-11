"use client"

import { Webhooks } from "@/views/server/ResourceTabs"
import { useOrgParams } from "@/lib/params"

export default function Page() {
  const { org, server } = useOrgParams()
  return <Webhooks org={org} server={server} />
}
