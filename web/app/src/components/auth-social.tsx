// Shared building blocks for the sign-in / sign-up cards: the labelled
// divider, the brand-marked provider button, and the provider→logo map.
// Used by both /login and /register so the two stay in lockstep.

import { KeyRound } from "lucide-react"

import { GithubIcon, GoogleIcon, MicrosoftIcon } from "@/components/brand-icons"
import { Button } from "@/components/ui/button"
import { type SsoProviderInfo } from "@/lib/api"

/// A provider button: the brand mark pinned to the left, the label kept
/// centred — the shape people recognise from "Continue with …" flows.
export function ProviderButton({
  icon,
  label,
  onClick,
  disabled,
}: {
  icon: React.ReactNode
  label: string
  onClick: () => void
  disabled?: boolean
}) {
  return (
    <Button
      type="button"
      variant="outline"
      className="relative w-full"
      onClick={onClick}
      disabled={disabled}
    >
      <span className="absolute left-4 flex items-center">{icon}</span>
      {label}
    </Button>
  )
}

/// Match a social provider to its brand mark. Falls back to a neutral key
/// icon for any provider we don't have a logo for.
export function providerIcon(provider: SsoProviderInfo): React.ReactNode {
  const hint = `${provider.id} ${provider.name}`.toLowerCase()
  if (provider.type === "github" || hint.includes("github")) return <GithubIcon />
  if (hint.includes("microsoft") || hint.includes("azure") || hint.includes("entra"))
    return <MicrosoftIcon />
  if (hint.includes("google")) return <GoogleIcon />
  return <KeyRound />
}

/// A labelled divider ("Or continue with") separating the password form
/// from the alternative sign-in methods.
export function AuthDivider({ label = "Or continue with" }: { label?: string }) {
  return (
    <div className="flex items-center gap-3 py-1 text-xs text-muted-foreground">
      <span className="h-px grow bg-border" />
      {label}
      <span className="h-px grow bg-border" />
    </div>
  )
}
