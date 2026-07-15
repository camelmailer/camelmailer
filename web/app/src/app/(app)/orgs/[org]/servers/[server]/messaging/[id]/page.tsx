"use client"

import { useParams } from "next/navigation"
import { MessageDetailPage, useMessagingApi } from "@/views/server/Messaging"
import { useOrgParams } from "@/lib/params"

export default function Page() {
  // The messaging layout wraps this route in <MessagingShell>, so the
  // server messaging API context is already provided.
  const api = useMessagingApi()
  const { org, server } = useOrgParams()
  const params = useParams<{ id?: string }>()
  const id = Number(params?.id)
  return <MessageDetailPage api={api} org={org} server={server} id={id} />
}
