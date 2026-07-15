"use client"

import { ServerStreams } from "@/views/server/Messaging"
import { useOrgParams } from "@/lib/params"

export default function Page() {
  const { org, server } = useOrgParams()
  return <ServerStreams org={org} server={server} />
}
