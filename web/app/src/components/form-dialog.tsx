"use client"

// The unified create/edit modal: title, fields as children, submit on
// ⌘↵, cancel on Esc, and a busy state. The shortcut hints live in the
// command palette / search, not on these buttons. `Kbd` stays exported
// for those surfaces.

import type { ReactNode } from "react"
import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { cn } from "@/lib/utils"

export function Kbd({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <kbd
      className={cn(
        "pointer-events-none inline-flex h-4 min-w-4 items-center justify-center rounded border bg-muted px-1 font-mono text-[10px] font-medium text-muted-foreground",
        className,
      )}
    >
      {children}
    </kbd>
  )
}

export function FormDialog({
  open,
  onOpenChange,
  title,
  description,
  children,
  submitLabel = "Create",
  onSubmit,
  busy = false,
  submitDisabled = false,
  showSubmit = true,
  cancelLabel = "Cancel",
  wide = false,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  title: string
  description?: string
  children: ReactNode
  submitLabel?: string
  onSubmit: () => void
  busy?: boolean
  submitDisabled?: boolean
  /** false while e.g. a one-time secret is shown — hides the submit button. */
  showSubmit?: boolean
  cancelLabel?: string
  wide?: boolean
}) {
  const canSubmit = showSubmit && !busy && !submitDisabled
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        className={wide ? "sm:max-w-2xl" : undefined}
        onKeyDown={(e) => {
          if ((e.metaKey || e.ctrlKey) && e.key === "Enter" && canSubmit) {
            e.preventDefault()
            onSubmit()
          }
        }}
      >
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          {description && <DialogDescription>{description}</DialogDescription>}
        </DialogHeader>
        {children}
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            {cancelLabel}
          </Button>
          {showSubmit && (
            <Button onClick={onSubmit} disabled={busy || submitDisabled}>
              {busy ? "Working…" : submitLabel}
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
