"use client"

import { Suspense } from "react"
import ConfirmSenderAddress from "@/views/auth/ConfirmSenderAddress"
export default function Page() {
  return (
    <Suspense>
      <ConfirmSenderAddress />
    </Suspense>
  )
}
