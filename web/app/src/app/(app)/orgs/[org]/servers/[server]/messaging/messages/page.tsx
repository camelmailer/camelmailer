"use client"

import { Messages, useMessagingApi } from "@/views/server/Messaging"

export default function Page() {
  const api = useMessagingApi()
  return <Messages api={api} />
}
