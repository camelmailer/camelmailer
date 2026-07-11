"use client"

import { Credentials } from "@/views/server/ResourceTabs"
import { useOrgParams } from "@/lib/params"

export default function Page() {
  const { org, server } = useOrgParams()
  return <Credentials org={org} server={server} />
}
