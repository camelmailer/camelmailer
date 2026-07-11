"use client"

import { Suspense } from "react"
import AcceptInvitation from "@/views/auth/AcceptInvitation"
export default function Page() {
  return (
    <Suspense>
      <AcceptInvitation />
    </Suspense>
  )
}
