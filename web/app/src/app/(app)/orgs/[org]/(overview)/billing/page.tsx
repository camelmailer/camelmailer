"use client"

import { BillingView } from "@/views/org/Billing"
import { useOrgParams } from "@/lib/params"

export default function Page() {
  const { org } = useOrgParams()
  return <BillingView org={org} />
}
