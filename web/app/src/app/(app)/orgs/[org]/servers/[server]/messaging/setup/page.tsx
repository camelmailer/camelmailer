"use client"

import { SetupTab } from "@/views/server/SetupTab"
import { useOrgParams } from "@/lib/params"

export default function Page() {
  const { org, server } = useOrgParams()
  return <SetupTab org={org} server={server} />
}
