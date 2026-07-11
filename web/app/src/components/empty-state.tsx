"use client"

// The empty-state pattern used across all list views: no dead empty
// tables — an icon in a soft circle, "No X yet", one sentence of value,
// and an immediately actionable CTA (optionally a secondary one and a
// copyable value / snippet as children).

import Link from "next/link"
import type { ReactNode } from "react"
import type { LucideIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Card, CardContent } from "@/components/ui/card"

export type EmptyStateAction = {
  label: string
  onClick?: () => void
  href?: string
}

function ActionButton({
  action,
  variant,
}: {
  action: EmptyStateAction
  variant: "default" | "outline"
}) {
  if (action.href) {
    return (
      <Button asChild variant={variant} size="sm">
        <Link href={action.href}>{action.label}</Link>
      </Button>
    )
  }
  return (
    <Button variant={variant} size="sm" onClick={action.onClick}>
      {action.label}
    </Button>
  )
}

export function EmptyState({
  icon: Icon,
  title,
  description,
  action,
  secondaryAction,
  children,
}: {
  icon: LucideIcon
  title: string
  description: string
  action?: EmptyStateAction
  secondaryAction?: EmptyStateAction
  children?: ReactNode
}) {
  return (
    <Card>
      <CardContent className="flex flex-col items-center gap-3 py-12 text-center">
        <div className="flex size-12 items-center justify-center rounded-full bg-muted">
          <Icon className="size-5 text-muted-foreground" />
        </div>
        <div className="space-y-1">
          <h3 className="text-sm font-medium">{title}</h3>
          <p className="mx-auto max-w-sm text-sm text-muted-foreground">{description}</p>
        </div>
        {(action || secondaryAction) && (
          <div className="mt-1 flex flex-wrap items-center justify-center gap-2">
            {action && <ActionButton action={action} variant="default" />}
            {secondaryAction && <ActionButton action={secondaryAction} variant="outline" />}
          </div>
        )}
        {children}
      </CardContent>
    </Card>
  )
}
