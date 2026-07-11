"use client"

import { Templates, useMessagingApi } from "@/views/server/Messaging"

export default function Page() {
  const api = useMessagingApi()
  return <Templates api={api} />
}
