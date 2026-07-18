"use client"

// The `</>` API panel (masterplan §5): a global slide-over that turns the
// resource page you are on into copy-paste-ready API snippets — the
// UI-as-API twin. It reads the active route, picks the matching endpoint
// from the (OpenAPI-shaped) catalog in api-p4.ts, and renders it in cURL /
// Node / Python / Go with a persisted language choice.

import { useMemo, useState, type ReactNode } from "react"
import { useParams, usePathname } from "next/navigation"
import { useQuery } from "@tanstack/react-query"
import {
  ArrowUpRightIcon,
  BookOpenIcon,
  CodeXmlIcon,
  CompassIcon,
  RocketIcon,
} from "lucide-react"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
  SheetTrigger,
} from "@/components/ui/sheet"
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { CopyButton } from "@/components/shared"
import { adminApi } from "@/lib/api"
import {
  CODE_LANGS,
  endpointCatalog,
  endpointHasResource,
  KEY_PLACEHOLDER,
  loadCodeLang,
  renderSnippet,
  resolvePath,
  resolveApiSection,
  saveCodeLang,
  type CodeContext,
  type CodeLang,
} from "@/lib/api-p4"

const DOCS = "https://camelmailer.com/docs"

/** SDK hub entries mirrored from camelmailer.com/docs/sdks. */
const SDKS: { slug: string; label: string }[] = [
  { slug: "node", label: "Node" },
  { slug: "python", label: "Python" },
  { slug: "php", label: "PHP" },
  { slug: "laravel", label: "Laravel" },
  { slug: "go", label: "Go" },
  { slug: "ruby", label: "Ruby" },
  { slug: "rust", label: "Rust" },
  { slug: "java", label: "Java" },
  { slug: "dotnet", label: ".NET" },
  { slug: "cli", label: "CLI" },
  { slug: "mcp", label: "MCP" },
]

const METHOD_CLASSES: Record<string, string> = {
  GET: "border-sky-600/30 bg-sky-600/10 text-sky-700 dark:border-sky-400/30 dark:bg-sky-400/10 dark:text-sky-400",
  POST: "border-green-600/30 bg-green-600/10 text-green-700 dark:border-green-400/30 dark:bg-green-400/10 dark:text-green-400",
  PATCH:
    "border-amber-600/30 bg-amber-600/10 text-amber-700 dark:border-amber-400/30 dark:bg-amber-400/10 dark:text-amber-400",
  DELETE:
    "border-red-600/30 bg-red-600/10 text-red-700 dark:border-red-400/30 dark:bg-red-400/10 dark:text-red-400",
}

/// The global `</>` trigger + slide-over. Mounted once in the app header;
/// it derives everything from the current route, so it needs no props.
export function CodePanel() {
  const pathname = usePathname() ?? ""
  const params = useParams()
  const [open, setOpen] = useState(false)
  const [lang, setLang] = useState<CodeLang>("curl")
  // "this record" (real id) vs the reusable "generic" call.
  const [generic, setGeneric] = useState(false)
  const [apiKey, setApiKey] = useState<string>("")

  const org = typeof params?.org === "string" ? params.org : ""
  const server = typeof params?.server === "string" ? params.server : ""
  const str = (v: unknown) => (typeof v === "string" ? v : undefined)
  const permalink = str(params?.permalink)
  const id = str(params?.id)
  const name = str(params?.name)
  const email = str(params?.email)

  // Only meaningful inside a server's resource pages.
  const onResource = pathname.includes("/servers/")

  const endpoint = useMemo(() => {
    const section = resolveApiSection(pathname)
    return endpointCatalog()[section]
  }, [pathname])

  const hasResource = endpointHasResource(endpoint)

  // Server-auth endpoints authenticate with an API credential; offer the
  // server's real API keys so the snippet is copy-paste-runnable.
  const credentials = useQuery({
    queryKey: ["sapi-credentials-for-code", org, server],
    queryFn: () => adminApi.credentials(org, server).list(),
    enabled: open && endpoint.auth === "server" && !!org && !!server,
  })
  const apiKeys = useMemo(
    () => (credentials.data?.credentials ?? []).filter((c) => c.type === "API" && !c.hold),
    [credentials.data],
  )

  const ctx: CodeContext = useMemo(
    () => ({
      baseUrl:
        typeof window === "undefined" ? "https://mail.example.com" : window.location.origin,
      org,
      server,
      permalink,
      id,
      name,
      email,
      generic: hasResource ? generic : false,
    }),
    [org, server, permalink, id, name, email, generic, hasResource],
  )
  const snippet = useMemo(() => {
    const raw = renderSnippet(endpoint, lang, ctx)
    return apiKey ? raw.split(KEY_PLACEHOLDER).join(apiKey) : raw
  }, [endpoint, lang, ctx, apiKey])
  const displayPath = useMemo(() => resolvePath(endpoint, ctx), [endpoint, ctx])

  if (!onResource) return null

  return (
    <Sheet
      open={open}
      onOpenChange={(next) => {
        if (next) setLang(loadCodeLang())
        setOpen(next)
      }}
    >
      <SheetTrigger asChild>
        <Button
          variant="outline"
          size="sm"
          className="gap-1.5 font-mono text-muted-foreground"
          aria-label="Show API snippet for this page"
        >
          <CodeXmlIcon className="size-3.5" />
          <span className="hidden sm:inline">API</span>
        </Button>
      </SheetTrigger>
      <SheetContent className="flex w-full flex-col gap-0 sm:max-w-2xl">
        <SheetHeader>
          <SheetTitle className="flex items-center gap-2">
            <Badge variant="outline" className={METHOD_CLASSES[endpoint.method]}>
              {endpoint.method}
            </Badge>
            {endpoint.label}
          </SheetTitle>
          <SheetDescription>{endpoint.description}</SheetDescription>
        </SheetHeader>

        <div className="min-h-0 flex-1 space-y-4 overflow-y-auto px-4 pb-6">
          {/* This record ↔ generic call */}
          {hasResource && (
            <Tabs value={generic ? "generic" : "record"} onValueChange={(v) => setGeneric(v === "generic")}>
              <TabsList className="w-full">
                <TabsTrigger value="record" className="flex-1">
                  This {resourceNoun(endpoint.id)}
                </TabsTrigger>
                <TabsTrigger value="generic" className="flex-1">
                  Generic call
                </TabsTrigger>
              </TabsList>
            </Tabs>
          )}

          <code className="block break-all rounded-md bg-muted px-2 py-1.5 font-mono text-xs text-muted-foreground">
            {displayPath}
          </code>

          {/* API key picker (server-auth endpoints) */}
          {endpoint.auth === "server" && (
            <div className="grid gap-1.5">
              <label className="text-xs font-medium text-muted-foreground">API key</label>
              <Select value={apiKey || "__placeholder"} onValueChange={(v) => setApiKey(v === "__placeholder" ? "" : v)}>
                <SelectTrigger className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="__placeholder">Use a placeholder (cm_your_api_key)</SelectItem>
                  {apiKeys.map((c) => (
                    <SelectItem key={c.id} value={c.key}>
                      {c.name} · …{c.key.slice(-4)}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              {apiKeys.length === 0 && !credentials.isPending && (
                <p className="text-xs text-muted-foreground">
                  No API credentials yet — create one under Credentials to drop a live key in here.
                </p>
              )}
            </div>
          )}

          <Tabs
            value={lang}
            onValueChange={(value) => {
              setLang(value as CodeLang)
              saveCodeLang(value as CodeLang)
            }}
          >
            <TabsList className="flex-wrap">
              {CODE_LANGS.map((l) => (
                <TabsTrigger key={l.value} value={l.value}>
                  {l.label}
                </TabsTrigger>
              ))}
            </TabsList>
          </Tabs>

          <div className="relative">
            <div className="absolute right-2 top-2 z-10">
              <CopyButton value={snippet} />
            </div>
            <pre className="overflow-x-auto rounded-md bg-muted p-3 pr-10 font-mono text-xs leading-relaxed">
              {snippet}
            </pre>
          </div>

          {!apiKey && (
            <p className="text-xs text-muted-foreground">
              Replace <code className="font-mono">cm_your_api_key</code> with{" "}
              {endpoint.auth === "server"
                ? "a server API credential (X-Server-API-Key)"
                : "an admin API key or user bearer token (Authorization: Bearer)"}
              .
            </p>
          )}

          {/* Build faster: SDKs + docs */}
          <div className="space-y-3 border-t pt-4">
            <div>
              <p className="mb-2 text-xs font-medium text-foreground">Use a client SDK</p>
              <div className="flex flex-wrap gap-1.5">
                {SDKS.map((s) => (
                  <a
                    key={s.slug}
                    href={`${DOCS}/sdks/${s.slug}`}
                    target="_blank"
                    rel="noreferrer"
                    className="rounded-md border px-2 py-1 text-xs text-muted-foreground transition-colors hover:border-primary hover:text-foreground"
                  >
                    {s.label}
                  </a>
                ))}
              </div>
            </div>
            <div className="grid gap-1">
              <PanelLink href={`${DOCS}/quickstart`} icon={RocketIcon}>
                Quickstart guide
              </PanelLink>
              <PanelLink href={`${DOCS}/api`} icon={BookOpenIcon}>
                Full API reference
              </PanelLink>
              <PanelLink href={`${DOCS}/api/explorer`} icon={CompassIcon}>
                Interactive API explorer
              </PanelLink>
              <PanelLink href="/openapi.yaml" icon={CodeXmlIcon}>
                OpenAPI spec (openapi.yaml)
              </PanelLink>
            </div>
          </div>
        </div>
      </SheetContent>
    </Sheet>
  )
}

/** A friendly noun for the "This <noun>" toggle, from the endpoint id. */
function resourceNoun(id: string): string {
  const map: Record<string, string> = {
    message: "message",
    recipient: "recipient",
    stream: "stream",
    campaign: "campaign",
    template: "template",
    layout: "layout",
    domain: "domain",
    credential: "credential",
    webhook: "webhook",
  }
  return map[id] ?? "record"
}

function PanelLink({
  href,
  icon: Icon,
  children,
}: {
  href: string
  icon: typeof RocketIcon
  children: ReactNode
}) {
  return (
    <a
      href={href}
      target="_blank"
      rel="noreferrer"
      className="group flex items-center gap-2 rounded-md px-2 py-1.5 text-sm text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
    >
      <Icon className="size-4 shrink-0" />
      <span className="flex-1">{children}</span>
      <ArrowUpRightIcon className="size-3.5 opacity-0 transition-opacity group-hover:opacity-100" />
    </a>
  )
}
