"use client"

import { ServerSettingsPage } from "@/views/server/ServerHome"
import { useOrgParams } from "@/lib/params"

export default function Page() {
  const { org, server } = useOrgParams()
  return <ServerSettingsPage org={org} server={server} />
}
