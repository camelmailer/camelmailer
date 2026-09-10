"use client"

// Small shared building blocks used across the CRUD pages.

import { useState, type ReactNode } from "react"
import Link from "next/link"
import { CheckIcon, CopyIcon } from "lucide-react"
import { cn } from "@/lib/utils"
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"

export function PageHeader({
  title,
  description,
  action,
  backHref,
  backLabel,
  className,
}: {
  title: ReactNode
  description?: ReactNode
  action?: ReactNode
  // A muted, breadcrumb-style link rendered before the title ("Messages / …"),
  // used on detail pages instead of a separate back link.
  backHref?: string
  backLabel?: ReactNode
  className?: string
}) {
  return (
    <div
      className={cn("mb-4 flex flex-wrap items-center justify-between gap-3", className)}
    >
      <div className="min-w-0">
        <h1 className="text-lg font-semibold">
          {backHref && (
            <>
              <Link
                href={backHref}
                className="font-normal text-muted-foreground/70 transition-colors hover:text-foreground"
              >
                {backLabel}
              </Link>
              <span className="mx-1.5 font-normal text-muted-foreground/40">/</span>
            </>
          )}
          {title}
        </h1>
        {description && (
          <div className="mt-0.5 text-sm text-muted-foreground">{description}</div>
        )}
      </div>
      {action && <div className="flex shrink-0 items-center gap-2">{action}</div>}
    </div>
  )
}

export function EmptyState({ children }: { children: ReactNode }) {
  return (
    <div className="rounded-lg border border-dashed p-8 text-center text-sm text-muted-foreground">
      {children}
    </div>
  )
}

export function CopyButton({ value }: { value: string }) {
  const [copied, setCopied] = useState(false)
  return (
    <Button
      variant="ghost"
      size="icon"
      className="size-6"
      onClick={() => {
        navigator.clipboard.writeText(value)
        setCopied(true)
        setTimeout(() => setCopied(false), 1500)
      }}
    >
      {copied ? <CheckIcon className="size-3.5" /> : <CopyIcon className="size-3.5" />}
    </Button>
  )
}

/// One-time display of a freshly created secret (API key, invite link…).
export function SecretReveal({ label, value }: { label: string; value: string }) {
  return (
    <Alert>
      <AlertTitle>{label}</AlertTitle>
      <AlertDescription>
        <div className="flex w-full items-center gap-2">
          <code className="min-w-0 flex-1 break-all rounded bg-muted px-2 py-1 text-xs">
            {value}
          </code>
          <CopyButton value={value} />
        </div>
        <p className="mt-1 text-xs">You see this only once, so copy it now.</p>
      </AlertDescription>
    </Alert>
  )
}

export function ConfirmDialog({
  open,
  onOpenChange,
  title,
  description,
  confirmLabel = "Delete",
  confirmWord,
  onConfirm,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  title: string
  description: string
  confirmLabel?: string
  /// When set, the exact word has to be typed before the action unlocks.
  /// Reserved for deletions reaching data the caller cannot see from
  /// where they stand, such as an organization deleted from the admin
  /// list rather than from inside it.
  confirmWord?: string
  onConfirm: () => void | Promise<void>
}) {
  const [busy, setBusy] = useState(false)
  const [typed, setTyped] = useState("")
  const unlocked = !confirmWord || typed.trim() === confirmWord

  const run = async () => {
    if (!unlocked) return
    setBusy(true)
    try {
      await onConfirm()
      onOpenChange(false)
    } finally {
      setBusy(false)
    }
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) setTyped("")
        onOpenChange(next)
      }}
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription>{description}</DialogDescription>
        </DialogHeader>
        {confirmWord && (
          <div className="grid gap-2">
            <Label htmlFor="confirm-word">
              Type <span className="font-mono font-medium">{confirmWord}</span> to confirm
            </Label>
            <Input
              id="confirm-word"
              value={typed}
              autoComplete="off"
              onChange={(event) => setTyped(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") void run()
              }}
            />
          </div>
        )}
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button variant="destructive" disabled={busy || !unlocked} onClick={run}>
            {confirmLabel}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/// Human-readable timestamp for API dates.
export function formatDate(value: string | null | undefined): string {
  if (!value) return "—"
  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString()
}
