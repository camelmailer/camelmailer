// A full-height page scaffold: a fixed header with a hairline bottom border,
// and a body that scrolls on its own — so the header (and, for tables, the
// pagination) stay put while only the content moves. It fills the app content
// box (which pads with p-6) and bleeds that padding sideways so the header
// border runs edge to edge, like the breadcrumb row above it.

import type { ReactNode } from "react"
import { cn } from "@/lib/utils"

export function Page({
  header,
  children,
  variant = "scroll",
  className,
}: {
  header: ReactNode
  children: ReactNode
  // "scroll": the body scrolls (detail pages, settings, dashboards).
  // "fill": the body is a flex column that does not scroll itself — for a
  // <DataTable fillHeight>, whose own rows scroll and whose pagination stays
  // pinned at the bottom of the page.
  variant?: "scroll" | "fill"
  className?: string
}) {
  return (
    <div className={cn("flex h-full min-h-0 min-w-0 flex-col", className)}>
      <div className="-mx-6 shrink-0 border-b px-6 pb-4">{header}</div>
      {variant === "fill" ? (
        <div className="-mx-6 flex min-h-0 min-w-0 flex-1 flex-col px-6 pt-4">{children}</div>
      ) : (
        <div className="app-scrollbar -mx-6 min-h-0 min-w-0 flex-1 overflow-y-auto px-6 pt-4">
          {children}
        </div>
      )}
    </div>
  )
}
