"use client"

import { Send, useMessagingApi } from "@/views/server/Messaging"

export default function Page() {
  const api = useMessagingApi()
  return <Send api={api} />
}
