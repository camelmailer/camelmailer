"use client"

import { CampaignForm } from "@/views/server/Campaigns"
import { useOrgParams } from "@/lib/params"

export default function Page() {
  const { org, server } = useOrgParams()
  return <CampaignForm org={org} server={server} />
}
