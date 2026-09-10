"use client"

import { useParams } from "next/navigation"
import OrganizationDetail from "@/views/admin/OrganizationDetail"

// The segment is deliberately not called `org`: the app shell reads an
// `org` route param to decide which organization the sidebar and the
// switcher work against (and remembers it), which must not happen while
// an administrator inspects a tenant they are not a member of.
export default function Page() {
  const params = useParams<{ permalink?: string }>()
  const permalink = decodeURIComponent((params?.permalink as string) ?? "")
  return <OrganizationDetail org={permalink} />
}
