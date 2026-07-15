"use client"

import { useParams } from "next/navigation"
import { CredentialDetail } from "@/views/server/ResourceTabs"
import { useOrgParams } from "@/lib/params"

export default function Page() {
  const { org, server } = useOrgParams()
  const params = useParams<{ id?: string }>()
  return <CredentialDetail org={org} server={server} id={Number(params?.id)} />
}
