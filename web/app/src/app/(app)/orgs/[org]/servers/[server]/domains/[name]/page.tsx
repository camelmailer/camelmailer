"use client"

import { useParams } from "next/navigation"
import { DomainDetail } from "@/views/server/DomainDetail"
import { useOrgParams } from "@/lib/params"

export default function Page() {
  const { org, server } = useOrgParams()
  const params = useParams<{ name?: string }>()
  const name = decodeURIComponent((params?.name as string) ?? "")
  return <DomainDetail org={org} server={server} name={name} />
}
