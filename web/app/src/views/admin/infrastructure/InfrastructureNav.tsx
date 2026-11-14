"use client"

// The sub-navigation of the infrastructure area. One line per section: a
// new section (IP metrics and warm-up, provider feedback) adds its entry
// here and its page under src/app/(app)/admin/infrastructure/.

import Link from "next/link"
import { usePathname } from "next/navigation"
import { cn } from "@/lib/utils"

export const INFRASTRUCTURE_SECTIONS: readonly (readonly [href: string, label: string])[] = [
  ["/admin/infrastructure", "Topology"],
  ["/admin/infrastructure/dns", "DNS checks"],
  ["/admin/infrastructure/ips", "IP metrics & warm-up"],
]

export function InfrastructureNav() {
  const pathname = usePathname()
  return (
    <nav className="mt-3 flex flex-wrap gap-1" aria-label="Infrastructure sections">
      {INFRASTRUCTURE_SECTIONS.map(([href, label]) => {
        const active = pathname === href
        return (
          <Link
            key={href}
            href={href}
            aria-current={active ? "page" : undefined}
            className={cn(
              "rounded-md px-3 py-1.5 text-sm font-medium transition-colors",
              active
                ? "bg-muted text-foreground"
                : "text-muted-foreground hover:bg-muted/60 hover:text-foreground",
            )}
          >
            {label}
          </Link>
        )
      })}
    </nav>
  )
}
