"use client"

import { RecipientsList } from "@/views/server/RecipientDetail"
import { useOrgParams } from "@/lib/params"

export default function Page() {
  const { org, server } = useOrgParams()
  return <RecipientsList org={org} server={server} />
}
