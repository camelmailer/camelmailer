"use client"

// There is no standalone recipient list — recipients are reached from
// the Messages list and the suppression list. The bare segment (e.g.
// via the breadcrumb) lands on Messages.

import { redirect } from "next/navigation"
import { useOrgParams } from "@/lib/params"

export default function Page() {
  const { org, server } = useOrgParams()
  redirect(`/orgs/${org}/servers/${server}/messaging/messages`)
}
