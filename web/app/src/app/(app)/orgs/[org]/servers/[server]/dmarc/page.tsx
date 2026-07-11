"use client"

import { Dmarc } from "@/views/server/Dmarc"
import { useOrgParams } from "@/lib/params"

export default function Page() {
  const { org, server } = useOrgParams()
  return <Dmarc org={org} server={server} />
}
