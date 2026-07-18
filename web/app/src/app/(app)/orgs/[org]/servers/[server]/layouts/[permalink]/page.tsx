"use client"

import { useParams } from "next/navigation"
import { MessagingApiProvider, useMessagingApi } from "@/views/server/Messaging"
import { LayoutEditor } from "@/views/server/LayoutEditor"
import { useOrgParams } from "@/lib/params"

function Editor({
  org,
  server,
  permalink,
}: {
  org: string
  server: string
  permalink: string | null
}) {
  const api = useMessagingApi()
  return <LayoutEditor api={api} org={org} server={server} permalink={permalink} />
}

export default function Page() {
  const { org, server } = useOrgParams()
  const params = useParams<{ permalink?: string }>()
  const permalink = params?.permalink ? decodeURIComponent(params.permalink as string) : null
  return (
    <MessagingApiProvider org={org} server={server}>
      <Editor org={org} server={server} permalink={permalink} />
    </MessagingApiProvider>
  )
}
