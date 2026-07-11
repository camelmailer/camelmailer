"use client"

import { StatsView, useMessagingApi } from "@/views/server/Messaging"

export default function Page() {
  const api = useMessagingApi()
  return <StatsView api={api} />
}
