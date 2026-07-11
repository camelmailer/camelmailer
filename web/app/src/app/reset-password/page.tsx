"use client"

import { Suspense } from "react"
import ResetPassword from "@/views/auth/ResetPassword"
export default function Page() {
  return (
    <Suspense>
      <ResetPassword />
    </Suspense>
  )
}
