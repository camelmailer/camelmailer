"use client"

import { useParams } from "next/navigation"
import { TemplateEditor } from "@/views/server/TemplateEditor"
import { useMessagingApi } from "@/views/server/Messaging"
import { useOrgParams } from "@/lib/params"

export default function Page() {
  const api = useMessagingApi()
  const { org, server } = useOrgParams()
  const params = useParams<{ permalink?: string }>()
  const permalink = params?.permalink ? decodeURIComponent(params.permalink as string) : null
  return <TemplateEditor api={api} org={org} server={server} permalink={permalink} />
}
