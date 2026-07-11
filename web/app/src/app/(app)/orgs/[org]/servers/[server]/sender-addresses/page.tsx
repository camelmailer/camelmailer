"use client"

import { SenderAddresses } from "@/views/server/ResourceTabs"
import { useOrgParams } from "@/lib/params"

export default function Page() {
  const { org, server } = useOrgParams()
  return <SenderAddresses org={org} server={server} />
}
