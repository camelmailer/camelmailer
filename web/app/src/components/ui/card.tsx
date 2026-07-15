"use client"

import * as React from "react"

import { cn } from "@/lib/utils"

function Card({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="card"
      className={cn(
        "flex flex-col gap-6 rounded-xl border bg-card py-6 text-card-foreground shadow-sm",
        className
      )}
      {...props}
    />
  )
}

/// The glassy "halo" card — the pricing-box look: an outer glass-rimmed
/// ring, a padded middle forming a concentric halo gap, and the inner
/// card surface (keeps data-slot=card, so the Card* subcomponents work
/// inside it). Reserved for spotlight surfaces (auth screens, the
/// lightbox/dialog) rather than the dense dashboard. `className` targets
/// the outer frame, so layout classes like `w-full` propagate.
function HaloCard({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      className={cn(
        // Outer radius = inner radius + gap, so the frame stays concentric
        // with the inner card (inner 1.5rem, gap 6px -> outer 1.875rem).
        "-m-1.5 grid grid-cols-1 rounded-[1.875rem] shadow-[inset_0_0_2px_1px_#ffffff4d] ring-1 ring-black/5 dark:shadow-[inset_0_0_2px_1px_rgba(255,255,255,0.12)] dark:ring-white/10",
        className
      )}
    >
      <div className="grid grid-cols-1 rounded-[1.875rem] p-1.5 shadow-md shadow-black/5">
        <div
          data-slot="card"
          className="flex flex-col gap-6 rounded-3xl bg-card py-6 text-card-foreground shadow-2xl ring-1 ring-black/5 dark:ring-white/10"
          {...props}
        />
      </div>
    </div>
  )
}

function CardHeader({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="card-header"
      className={cn(
        "@container/card-header grid auto-rows-min grid-rows-[auto_auto] items-start gap-2 px-6 has-data-[slot=card-action]:grid-cols-[1fr_auto] [.border-b]:pb-6",
        className
      )}
      {...props}
    />
  )
}

function CardTitle({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="card-title"
      className={cn("leading-none font-semibold", className)}
      {...props}
    />
  )
}

function CardDescription({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="card-description"
      className={cn("text-sm text-muted-foreground", className)}
      {...props}
    />
  )
}

function CardAction({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="card-action"
      className={cn(
        "col-start-2 row-span-2 row-start-1 self-start justify-self-end",
        className
      )}
      {...props}
    />
  )
}

function CardContent({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="card-content"
      className={cn("px-6", className)}
      {...props}
    />
  )
}

function CardFooter({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="card-footer"
      className={cn("flex items-center px-6 [.border-t]:pt-6", className)}
      {...props}
    />
  )
}

export {
  Card,
  HaloCard,
  CardHeader,
  CardFooter,
  CardTitle,
  CardAction,
  CardDescription,
  CardContent,
}
