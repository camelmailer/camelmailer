"use client"

import { ServerTemplates } from "@/views/server/Messaging"
import { useOrgParams } from "@/lib/params"

export default function Page() {
  const { org, server } = useOrgParams()
  return <ServerTemplates org={org} server={server} view="layouts" />
}
