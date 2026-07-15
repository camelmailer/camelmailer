"use client"

// The global ⌘K command palette: navigation (dashboard, org areas,
// servers of the active org and their subareas), actions (create
// organization, add domain, create API key) and org switching. cmdk
// provides the fuzzy matching over the item labels.

import { useEffect } from "react"
import { useParams, useRouter } from "next/navigation"
import { useQuery } from "@tanstack/react-query"
import {
  AtSignIcon,
  BanIcon,
  BuildingIcon,
  GlobeIcon,
  InboxIcon,
  KeyRoundIcon,
  LayoutDashboardIcon,
  MailIcon,
  PlusIcon,
  ServerIcon,
  SettingsIcon,
  ShieldCheckIcon,
  UsersIcon,
  WebhookIcon,
} from "lucide-react"
import {
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
  CommandSeparator,
} from "@/components/ui/command"
import { setLastActiveOrg } from "@/lib/api-extras"
import { adminApi } from "@/lib/api"
import { useAuth } from "@/lib/auth"

const SERVER_AREAS = [
  ["domains", "Domains", GlobeIcon],
  ["credentials", "Credentials", KeyRoundIcon],
  ["routes", "Routes", InboxIcon],
  ["webhooks", "Webhooks", WebhookIcon],
  ["sender-addresses", "Sender addresses", AtSignIcon],
  ["suppressions", "Suppressions", BanIcon],
  ["dmarc", "DMARC", ShieldCheckIcon],
  ["messaging", "Messages", MailIcon],
  ["templates", "Templates", MailIcon],
  ["settings", "Settings", SettingsIcon],
] as const

export function CommandPalette({
  open,
  onOpenChange,
  activeOrg,
  onCreateOrganization,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  activeOrg: string | undefined
  onCreateOrganization: () => void
}) {
  const { me } = useAuth()
  const router = useRouter()
  const params = useParams()
  const activeServer = typeof params?.server === "string" ? params.server : undefined

  // Same query key as the sidebar — the list is already warm.
  const servers = useQuery({
    queryKey: ["servers", activeOrg],
    queryFn: () => adminApi.servers(activeOrg!).list(),
    enabled: !!activeOrg,
  })

  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (e.key === "k" && (e.metaKey || e.ctrlKey)) {
        e.preventDefault()
        onOpenChange(!open)
      }
    }
    document.addEventListener("keydown", onKeyDown)
    return () => document.removeEventListener("keydown", onKeyDown)
  }, [open, onOpenChange])

  function run(action: () => void) {
    onOpenChange(false)
    action()
  }

  const memberships = me?.memberships ?? []
  const serverList = servers.data?.servers ?? []
  // Where "Add domain" / "Create API key" land: the server currently
  // open, otherwise the first server of the active org.
  const targetServer =
    (activeServer && serverList.find((s) => s.permalink === activeServer)) ??
    serverList[0]
  const orgBase = activeOrg ? `/orgs/${activeOrg}` : null

  return (
    <CommandDialog open={open} onOpenChange={onOpenChange}>
      <CommandInput placeholder="Type a command or search…" />
      <CommandList>
        <CommandEmpty>No results found.</CommandEmpty>
        <CommandGroup heading="Navigation">
          <CommandItem onSelect={() => run(() => router.push("/dashboard"))}>
            <LayoutDashboardIcon /> Dashboard
          </CommandItem>
          {orgBase && (
            <>
              <CommandItem onSelect={() => run(() => router.push(orgBase))}>
                <BuildingIcon /> {activeOrg}: Overview
              </CommandItem>
              <CommandItem onSelect={() => run(() => router.push(`${orgBase}/members`))}>
                <UsersIcon /> {activeOrg}: Members
              </CommandItem>
              <CommandItem onSelect={() => run(() => router.push(`${orgBase}/settings`))}>
                <SettingsIcon /> {activeOrg}: Settings
              </CommandItem>
            </>
          )}
        </CommandGroup>
        {orgBase && serverList.length > 0 && (
          <>
            <CommandSeparator />
            <CommandGroup heading="Servers">
              {serverList.map((server) => (
                <CommandItem
                  key={server.id}
                  value={`server ${server.name}`}
                  onSelect={() =>
                    run(() => router.push(`${orgBase}/servers/${server.permalink}`))
                  }
                >
                  <ServerIcon /> {server.name}
                </CommandItem>
              ))}
              {targetServer &&
                SERVER_AREAS.map(([path, label, Icon]) => (
                  <CommandItem
                    key={path}
                    value={`${targetServer.name} ${label}`}
                    onSelect={() =>
                      run(() =>
                        router.push(`${orgBase}/servers/${targetServer.permalink}/${path}`),
                      )
                    }
                  >
                    <Icon /> {targetServer.name}: {label}
                  </CommandItem>
                ))}
            </CommandGroup>
          </>
        )}
        <CommandSeparator />
        <CommandGroup heading="Actions">
          <CommandItem onSelect={() => run(onCreateOrganization)}>
            <PlusIcon /> Create organization
          </CommandItem>
          {orgBase && targetServer && (
            <>
              <CommandItem
                onSelect={() =>
                  run(() =>
                    router.push(`${orgBase}/servers/${targetServer.permalink}/domains`),
                  )
                }
              >
                <GlobeIcon /> Add domain
              </CommandItem>
              <CommandItem
                onSelect={() =>
                  run(() =>
                    router.push(`${orgBase}/servers/${targetServer.permalink}/credentials`),
                  )
                }
              >
                <KeyRoundIcon /> Create API key
              </CommandItem>
            </>
          )}
        </CommandGroup>
        {memberships.length > 0 && (
          <>
            <CommandSeparator />
            <CommandGroup heading="Switch organization">
              {memberships.map(({ organization }) => (
                <CommandItem
                  key={organization.id}
                  value={`org ${organization.name}`}
                  onSelect={() =>
                    run(() => {
                      setLastActiveOrg(organization.permalink)
                      router.push(`/orgs/${organization.permalink}`)
                    })
                  }
                >
                  <BuildingIcon /> {organization.name}
                </CommandItem>
              ))}
            </CommandGroup>
          </>
        )}
      </CommandList>
    </CommandDialog>
  )
}
