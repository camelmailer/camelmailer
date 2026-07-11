"use client"

// Dark mode: System / Light / Dark with localStorage + the class
// strategy on <html> (the shadcn CSS variables react to `.dark`).
// A tiny inline script in the root layout applies the stored choice
// before first paint to avoid a flash.

import { useEffect, useSyncExternalStore } from "react"
import { MonitorIcon, MoonIcon, SunIcon } from "lucide-react"
import {
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
} from "@/components/ui/dropdown-menu"

export type ThemePreference = "system" | "light" | "dark"

const THEME_KEY = "camelmailer.theme"
const THEME_EVENT = "camelmailer:theme"

export function getThemePreference(): ThemePreference {
  if (typeof window === "undefined") return "system"
  const stored = localStorage.getItem(THEME_KEY)
  return stored === "light" || stored === "dark" ? stored : "system"
}

function isDark(preference: ThemePreference): boolean {
  if (preference === "dark") return true
  if (preference === "light") return false
  return window.matchMedia("(prefers-color-scheme: dark)").matches
}

export function applyThemePreference(preference: ThemePreference) {
  if (typeof window === "undefined") return
  if (preference === "system") localStorage.removeItem(THEME_KEY)
  else localStorage.setItem(THEME_KEY, preference)
  document.documentElement.classList.toggle("dark", isDark(preference))
  window.dispatchEvent(new Event(THEME_EVENT))
}

function subscribe(callback: () => void): () => void {
  window.addEventListener("storage", callback)
  window.addEventListener(THEME_EVENT, callback)
  return () => {
    window.removeEventListener("storage", callback)
    window.removeEventListener(THEME_EVENT, callback)
  }
}

/// The "Theme" submenu for the NavUser dropdown.
export function ThemeSubMenu() {
  const preference = useSyncExternalStore(
    subscribe,
    getThemePreference,
    () => "system" as ThemePreference,
  )

  // Follow OS light/dark changes while on "system".
  useEffect(() => {
    const media = window.matchMedia("(prefers-color-scheme: dark)")
    const onChange = () => {
      if (getThemePreference() === "system") applyThemePreference("system")
    }
    media.addEventListener("change", onChange)
    return () => media.removeEventListener("change", onChange)
  }, [])

  const Icon =
    preference === "dark" ? MoonIcon : preference === "light" ? SunIcon : MonitorIcon

  return (
    <DropdownMenuSub>
      <DropdownMenuSubTrigger>
        <Icon className="mr-2 size-4 text-muted-foreground" /> Theme
      </DropdownMenuSubTrigger>
      <DropdownMenuSubContent>
        <DropdownMenuRadioGroup
          value={preference}
          onValueChange={(value) => applyThemePreference(value as ThemePreference)}
        >
          <DropdownMenuRadioItem value="system">
            <MonitorIcon className="mr-2 size-4" /> System
          </DropdownMenuRadioItem>
          <DropdownMenuRadioItem value="light">
            <SunIcon className="mr-2 size-4" /> Light
          </DropdownMenuRadioItem>
          <DropdownMenuRadioItem value="dark">
            <MoonIcon className="mr-2 size-4" /> Dark
          </DropdownMenuRadioItem>
        </DropdownMenuRadioGroup>
      </DropdownMenuSubContent>
    </DropdownMenuSub>
  )
}
