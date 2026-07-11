"use client"

import { useParams } from "next/navigation"

/// The [org] (and optionally [server]) segments as plain strings.
export function useOrgParams(): { org: string; server: string } {
  const params = useParams<{ org?: string; server?: string }>()
  return { org: (params?.org as string) ?? "", server: (params?.server as string) ?? "" }
}
