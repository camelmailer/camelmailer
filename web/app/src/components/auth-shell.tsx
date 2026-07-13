// The signed-out chrome shared by /login and /register. Inspired by the
// marketing site's "Radiant" look — the warm amber→orange gradient wash —
// but kept deliberately restrained: one soft glow behind a floating card,
// softer again in dark mode, so it reads as premium without competing
// with the form itself.

import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"

/// The warm gradient backdrop. Two blurred glows in the brand palette
/// (Cashmere #FFFDF1 → Butterscotch #FFCE99 → Whiskey #562F00), plus a
/// faint top wash tinted with the theme's primary so it stays on-brand in
/// both light and dark.
function AuthBackdrop() {
  return (
    <div aria-hidden className="pointer-events-none absolute inset-0 -z-10 overflow-hidden">
      <div className="absolute inset-0 bg-gradient-to-b from-primary/[0.05] to-transparent" />
      <div
        className="absolute -top-40 left-1/2 h-[32rem] w-[64rem] -translate-x-1/2 rounded-full opacity-20 blur-3xl dark:opacity-[0.12]"
        style={{ backgroundImage: "linear-gradient(115deg,#FFFDF1 28%,#FFCE99 70%,#562F00)" }}
      />
      <div
        className="absolute right-[-10%] -bottom-32 h-80 w-80 rounded-full opacity-10 blur-3xl dark:opacity-[0.07]"
        style={{ backgroundImage: "linear-gradient(115deg,#FFCE99,#562F00)" }}
      />
    </div>
  )
}

export function AuthShell({
  title,
  description,
  children,
  footer,
}: {
  title: string
  description: string
  children: React.ReactNode
  footer?: React.ReactNode
}) {
  return (
    <div className="relative flex min-h-svh flex-col items-center justify-center overflow-hidden p-4">
      <AuthBackdrop />
      <div className="flex w-full max-w-sm flex-col items-center">
        <div className="mb-6 flex items-center gap-2.5">
          {/* eslint-disable-next-line @next/next/no-img-element */}
          <img src="/camelmailer-symbol.png" alt="" className="size-8" />
          <span className="text-lg font-semibold tracking-tight">CamelMailer</span>
        </div>
        <Card className="w-full border-border/60 bg-card/80 shadow-xl backdrop-blur-sm">
          <CardHeader className="text-center">
            <CardTitle className="text-xl">{title}</CardTitle>
            <CardDescription>{description}</CardDescription>
          </CardHeader>
          <CardContent>{children}</CardContent>
        </Card>
        {footer ?? (
          <p className="mt-6 text-center text-xs text-muted-foreground">
            Transactional email. Nothing else.
          </p>
        )}
      </div>
    </div>
  )
}
