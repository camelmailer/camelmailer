"use client"

import { Routes } from "@/views/server/ResourceTabs"
import { useOrgParams } from "@/lib/params"

export default function Page() {
  const { org, server } = useOrgParams()
  return <Routes org={org} server={server} />
}
