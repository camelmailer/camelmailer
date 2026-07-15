// The signed-out chrome shared by /login and /register. Inspired by the
// marketing site's "Radiant" look — the warm amber→orange gradient wash —
// but kept deliberately restrained: one soft glow behind a floating card,
// softer again in dark mode, so it reads as premium without competing
// with the form itself.

import Link from "next/link"

import {
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
  HaloCard,
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

/// The Terms / Privacy consent line for the sign-in and registration
/// cards. Renders nothing unless the instance advertises legal links
/// (i.e. the hosted cloud); self-hosted installs stay clean.
export function AuthLegal({
  legal,
}: {
  legal?: { terms_url: string | null; privacy_url: string | null }
}) {
  const terms = legal?.terms_url
  const privacy = legal?.privacy_url
  if (!terms && !privacy) return null
  const link = (href: string, label: string) => (
    <a
      href={href}
      target="_blank"
      rel="noreferrer"
      className="underline underline-offset-2 hover:text-foreground"
    >
      {label}
    </a>
  )
  return (
    <p className="mt-6 max-w-sm text-center text-xs text-muted-foreground">
      By continuing you agree to our{" "}
      {terms && link(terms, "Terms")}
      {terms && privacy && " and "}
      {privacy && link(privacy, "Privacy Policy")}.
    </p>
  )
}

export function AuthShell({
  title,
  description,
  children,
  footer,
}: {
  title: string
  description: React.ReactNode
  children: React.ReactNode
  footer?: React.ReactNode
}) {
  return (
    <div className="relative flex min-h-svh flex-col items-center justify-center overflow-hidden p-4">
      <AuthBackdrop />
      <div className="flex w-full max-w-sm flex-col items-center">
        {/* The official wordmark lockup, always linking home to /login. It
            is a fixed dark-slate PNG, so in dark mode it is turned
            monochrome white to stay legible on the dark canvas. */}
        <Link href="/login" className="mb-6 inline-block rounded-sm">
          {/* eslint-disable-next-line @next/next/no-img-element */}
          <img
            src="/camelmailer-logo.png"
            alt="CamelMailer"
            className="h-8 w-auto dark:brightness-0 dark:invert"
          />
        </Link>
        <HaloCard className="w-full">
          <CardHeader className="text-center">
            <CardTitle className="text-xl">{title}</CardTitle>
            <CardDescription>{description}</CardDescription>
          </CardHeader>
          <CardContent>{children}</CardContent>
        </HaloCard>
        {footer}
      </div>
    </div>
  )
}
