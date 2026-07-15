"use client"

import * as React from "react"
import { cva, type VariantProps } from "class-variance-authority"
import { Slot } from "radix-ui"

import { cn } from "@/lib/utils"

// The glassy finish used by the default button, factored out for the other
// filled buttons: a diagonal white sheen (::before), an inset white
// highlight and a soft drop shadow. `before:rounded-[inherit]` makes the
// sheen follow whatever radius the button/size sets. Naturally near
// invisible on light fills (white-on-light) — most present on saturated
// surfaces, which is the intent.
const glassy =
  "relative shadow-xs inset-shadow-2xs inset-shadow-white/30 before:absolute before:inset-0 before:rounded-[inherit] before:bg-linear-to-bl before:from-white/30 before:to-transparent"

// A resting "card" surface (border + subtle shadow) that reveals the glassy
// finish only on hover / when its menu is open — the same interaction the
// sidebar items use. For the top-bar controls (search, admin, org, account).
export const interactiveCard =
  "relative rounded-lg border border-border bg-card shadow-sm before:pointer-events-none before:absolute before:inset-0 before:rounded-[inherit] before:bg-linear-to-bl before:from-white/40 before:to-transparent before:opacity-0 hover:inset-shadow-2xs hover:inset-shadow-white/40 hover:shadow-xs hover:before:opacity-100 data-[state=open]:bg-accent data-[state=open]:inset-shadow-2xs data-[state=open]:inset-shadow-white/40 data-[state=open]:shadow-xs data-[state=open]:before:opacity-100"

const buttonVariants = cva(
  "inline-flex shrink-0 cursor-pointer items-center justify-center gap-2 rounded-md text-sm font-medium whitespace-nowrap transition-all outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50 disabled:pointer-events-none disabled:opacity-50 aria-invalid:border-destructive aria-invalid:ring-destructive/20 dark:aria-invalid:ring-destructive/40 [&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-4",
  {
    variants: {
      variant: {
        default: `border-1 border-primary bg-primary text-primary-foreground hover:bg-primary/90 relative before:absolute
   inset-shadow-2xs inset-shadow-white/30
        shadow-md
        before:rounded-md
        before:inset-0 before:bg-linear-to-bl before:from-white/30
    before:to-transparent`,
        destructive: `border-1 border-destructive bg-destructive text-white hover:bg-destructive/90 focus-visible:ring-destructive/20 dark:bg-destructive/60 dark:focus-visible:ring-destructive/40 ${glassy}`,
        outline: `border bg-background hover:bg-accent hover:text-accent-foreground dark:border-input dark:bg-input/30 dark:hover:bg-input/50 ${glassy}`,
        secondary: `border-1 border-secondary bg-secondary text-secondary-foreground hover:bg-secondary/80 ${glassy}`,
        ghost: `hover:bg-accent hover:text-accent-foreground dark:hover:bg-accent/50`,
        link: "text-primary underline-offset-4 hover:underline",
      },
      size: {
        default: "h-9 px-4 py-2 has-[>svg]:px-3",
        xs: "h-6 gap-1 rounded-md px-2 text-xs has-[>svg]:px-1.5 [&_svg:not([class*='size-'])]:size-3",
        sm: "h-8 gap-1.5 rounded-md px-3 has-[>svg]:px-2.5",
        lg: "h-10 rounded-md px-6 has-[>svg]:px-4",
        icon: "size-9",
        "icon-xs": "size-6 rounded-md [&_svg:not([class*='size-'])]:size-3",
        "icon-sm": "size-8",
        "icon-lg": "size-10",
      },
    },
    defaultVariants: {
      variant: "default",
      size: "default",
    },
  }
)

function Button({
  className,
  variant = "default",
  size = "default",
  asChild = false,
  ...props
}: React.ComponentProps<"button"> &
  VariantProps<typeof buttonVariants> & {
    asChild?: boolean
  }) {
  const Comp = asChild ? Slot.Root : "button"

  return (
    <Comp
      data-slot="button"
      data-variant={variant}
      data-size={size}
      className={cn(buttonVariants({ variant, size, className }))}
      {...props}
    />
  )
}

export { Button, buttonVariants }
