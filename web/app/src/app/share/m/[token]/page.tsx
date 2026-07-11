"use client"

import { useParams } from "next/navigation"
import SharedMessage from "@/views/SharedMessage"

export default function Page() {
  const params = useParams<{ token?: string }>()
  return <SharedMessage token={(params?.token as string) ?? ""} />
}
