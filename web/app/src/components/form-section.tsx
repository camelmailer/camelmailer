// Two-column settings forms: on desktop each section shows its title and
// a short description on the left, and its fields on the right in a 6-column
// responsive grid; sections stack with a hairline divider between them. On
// mobile everything collapses to a single column. Built on our own Label +
// tokens so it matches the rest of the dashboard.

import type { ReactNode } from "react"
import { cn } from "@/lib/utils"
import { Label } from "@/components/ui/label"

/// Wraps a series of <FormSection> rows with even vertical rhythm.
export function FormSections({
  children,
  className,
}: {
  children: ReactNode
  className?: string
}) {
  return <div className={cn("space-y-10", className)}>{children}</div>
}

/// One labelled section: title + description on the left, fields (a grid of
/// <Field>s) on the right.
export function FormSection({
  title,
  description,
  children,
  className,
}: {
  title: ReactNode
  description?: ReactNode
  children: ReactNode
  className?: string
}) {
  return (
    <section
      className={cn(
        "grid grid-cols-1 gap-x-8 gap-y-6 border-b border-border pb-10 last:border-0 last:pb-0 md:grid-cols-3",
        className,
      )}
    >
      <div className="md:max-w-xs">
        <h2 className="text-base font-semibold">{title}</h2>
        {description && <p className="mt-1 text-sm text-muted-foreground">{description}</p>}
      </div>
      <div className="grid grid-cols-1 gap-x-6 gap-y-6 sm:grid-cols-6 md:col-span-2">
        {children}
      </div>
    </section>
  )
}

const SPAN: Record<2 | 3 | 4 | 6, string> = {
  2: "sm:col-span-2",
  3: "sm:col-span-3",
  4: "sm:col-span-4",
  6: "sm:col-span-6",
}

/// One control inside a <FormSection> grid: an optional label above the
/// control and an optional hint below it. `span` is out of 6 columns on
/// desktop (defaults to full width); always full width on mobile.
export function Field({
  label,
  htmlFor,
  hint,
  span = 6,
  className,
  children,
}: {
  label?: ReactNode
  htmlFor?: string
  hint?: ReactNode
  span?: 2 | 3 | 4 | 6
  className?: string
  children: ReactNode
}) {
  return (
    <div className={cn("col-span-full", SPAN[span], className)}>
      {label && (
        <Label htmlFor={htmlFor} className="mb-2 block">
          {label}
        </Label>
      )}
      {children}
      {hint && <p className="mt-2 text-sm text-muted-foreground">{hint}</p>}
    </div>
  )
}

/// A full-width row of buttons inside a section (e.g. a Save action).
export function FormActions({
  children,
  className,
}: {
  children: ReactNode
  className?: string
}) {
  return (
    <div className={cn("col-span-full flex flex-wrap items-center gap-3 pt-2", className)}>
      {children}
    </div>
  )
}
