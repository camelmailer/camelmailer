"use client"

import { useParams } from "next/navigation"
import { CampaignDetail, MessagingApiProvider, useMessagingApi } from "@/views/server/Messaging"
import { useOrgParams } from "@/lib/params"

function Detail({
  org,
  server,
  permalink,
  id,
}: {
  org: string
  server: string
  permalink: string
  id: number
}) {
  const api = useMessagingApi()
  return <CampaignDetail api={api} org={org} server={server} permalink={permalink} id={id} />
}

export default function Page() {
  const { org, server } = useOrgParams()
  const params = useParams<{ permalink?: string; id?: string }>()
  const permalink = params?.permalink ? decodeURIComponent(params.permalink as string) : ""
  const id = Number(params?.id ?? 0)
  return (
    <MessagingApiProvider org={org} server={server}>
      <Detail org={org} server={server} permalink={permalink} id={id} />
    </MessagingApiProvider>
  )
}
