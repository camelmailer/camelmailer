"use client"

import { useParams } from "next/navigation"
import { RecipientDetail } from "@/views/server/RecipientDetail"
import { useOrgParams } from "@/lib/params"

export default function Page() {
  const { org, server } = useOrgParams()
  const params = useParams<{ email?: string }>()
  const email = decodeURIComponent((params?.email as string) ?? "")
  return <RecipientDetail org={org} server={server} email={email} />
}
