"use client"

import { ServerLogs } from "@/views/server/Messaging"
import { useOrgParams } from "@/lib/params"

export default function Page() {
  const { org, server } = useOrgParams()
  return <ServerLogs org={org} server={server} />
}
