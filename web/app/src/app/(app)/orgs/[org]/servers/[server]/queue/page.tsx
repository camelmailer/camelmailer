"use client"

import { ServerQueue } from "@/views/server/Messaging"
import { useOrgParams } from "@/lib/params"

export default function Page() {
  const { org, server } = useOrgParams()
  return <ServerQueue org={org} server={server} />
}
