"use client"

import { Templates, useMessagingApi } from "@/views/server/Messaging"
import { useOrgParams } from "@/lib/params"

export default function Page() {
  const api = useMessagingApi()
  const { org, server } = useOrgParams()
  return <Templates api={api} org={org} server={server} />
}
