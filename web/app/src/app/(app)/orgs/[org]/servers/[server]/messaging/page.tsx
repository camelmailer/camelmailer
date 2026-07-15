"use client"

import { MessagingHome, useMessagingApi } from "@/views/server/Messaging"

export default function Page() {
  const api = useMessagingApi()
  return <MessagingHome api={api} />
}
