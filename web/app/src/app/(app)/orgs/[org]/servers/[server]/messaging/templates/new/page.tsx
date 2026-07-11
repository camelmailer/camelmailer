"use client"

import { TemplateEditor } from "@/views/server/TemplateEditor"
import { useMessagingApi } from "@/views/server/Messaging"
import { useOrgParams } from "@/lib/params"

export default function Page() {
  const api = useMessagingApi()
  const { org, server } = useOrgParams()
  return <TemplateEditor api={api} org={org} server={server} permalink={null} />
}
