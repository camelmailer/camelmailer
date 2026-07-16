"use client"

import { CampaignsList } from "@/views/server/Campaigns"
import { useOrgParams } from "@/lib/params"

export default function Page() {
  const { org, server } = useOrgParams()
  return <CampaignsList org={org} server={server} />
}
