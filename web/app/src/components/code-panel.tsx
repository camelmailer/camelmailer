"use client"

// The `</>` API panel (masterplan §5): a global slide-over that turns the
// resource page you are on into copy-paste-ready API snippets — the
// UI-as-API twin. It reads the active route, picks the matching endpoint
// from the (OpenAPI-shaped) catalog in api-p4.ts, and renders it in cURL /
// Node / Python / Go with a persisted language choice.

import { useMemo, useState } from "react"
import { useParams, usePathname } from "next/navigation"
import { CodeXmlIcon } from "lucide-react"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
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
import {
  CODE_LANGS,
  endpointCatalog,
  loadCodeLang,
  renderSnippet,
  resolveApiSection,
  saveCodeLang,
  type CodeContext,
  type CodeLang,
} from "@/lib/api-p4"

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

  const org = typeof params?.org === "string" ? params.org : ""
  const server = typeof params?.server === "string" ? params.server : ""

  // Only meaningful inside a server's resource pages.
  const onResource = pathname.includes("/servers/")

  const endpoint = useMemo(() => {
    const section = resolveApiSection(pathname)
    return endpointCatalog()[section]
  }, [pathname])

  const ctx: CodeContext = useMemo(
    () => ({
      baseUrl:
        typeof window === "undefined" ? "https://mail.example.com" : window.location.origin,
      org,
      server,
    }),
    [org, server],
  )
  const snippet = useMemo(() => renderSnippet(endpoint, lang, ctx), [endpoint, lang, ctx])

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
      <SheetContent className="flex w-full flex-col gap-0 sm:max-w-lg">
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
          <code className="block break-all rounded-md bg-muted px-2 py-1.5 font-mono text-xs text-muted-foreground">
            {endpoint.path}
          </code>

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

          <p className="text-xs text-muted-foreground">
            Replace <code className="font-mono">cm_your_api_key</code> with a server API
            credential. The full reference lives in{" "}
            <a href="/openapi.yaml" className="underline underline-offset-2" target="_blank" rel="noreferrer">
              openapi.yaml
            </a>
            .
          </p>
        </div>
      </SheetContent>
    </Sheet>
  )
}
