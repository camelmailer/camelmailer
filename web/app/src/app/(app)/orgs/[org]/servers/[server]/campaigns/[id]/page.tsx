"use client"

import { useParams } from "next/navigation"
import { CampaignDetailPage } from "@/views/server/Campaigns"
import { useOrgParams } from "@/lib/params"

export default function Page() {
  const { org, server } = useOrgParams()
  const params = useParams<{ id?: string }>()
  const id = Number(params?.id ?? 0)
  return <CampaignDetailPage org={org} server={server} id={id} />
}
