"use client"

import { Streams, useMessagingApi } from "@/views/server/Messaging"

export default function Page() {
  const api = useMessagingApi()
  return <Streams api={api} />
}
