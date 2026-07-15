"use client"

import { useParams } from "next/navigation"
import { MessagingApiProvider, StreamDetail, useMessagingApi } from "@/views/server/Messaging"
import { useOrgParams } from "@/lib/params"

function Detail({
  org,
  server,
  permalink,
}: {
  org: string
  server: string
  permalink: string
}) {
  const api = useMessagingApi()
  return <StreamDetail api={api} org={org} server={server} permalink={permalink} />
}

export default function Page() {
  const { org, server } = useOrgParams()
  const params = useParams<{ permalink?: string }>()
  const permalink = params?.permalink ? decodeURIComponent(params.permalink as string) : ""
  return (
    <MessagingApiProvider org={org} server={server}>
      <Detail org={org} server={server} permalink={permalink} />
    </MessagingApiProvider>
  )
}
