"use client"

import { InboundQueue, useMessagingApi } from "@/views/server/Messaging"

export default function Page() {
  const api = useMessagingApi()
  return <InboundQueue api={api} />
}
