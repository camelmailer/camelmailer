"use client"

import { MessagingApiProvider, useMessagingApi } from "@/views/server/Messaging"
import { TemplateEditor } from "@/views/server/TemplateEditor"
import { useOrgParams } from "@/lib/params"

function Editor({ org, server }: { org: string; server: string }) {
  const api = useMessagingApi()
  return <TemplateEditor api={api} org={org} server={server} permalink={null} />
}

export default function Page() {
  const { org, server } = useOrgParams()
  return (
    <MessagingApiProvider org={org} server={server}>
      <Editor org={org} server={server} />
    </MessagingApiProvider>
  )
}
