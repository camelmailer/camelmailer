"use client"

// Full-page layout editor, mirroring the template editor: a structured
// "Editor" mode (logo, brand, header color, footer) plus an "HTML" expert
// mode, with a live preview of a sample mail wrapped by the layout.

import { useMemo, useState } from "react"
import Link from "next/link"
import { useRouter } from "next/navigation"
import { useQuery } from "@tanstack/react-query"
import { toast } from "sonner"
import { ApiError, serverApi, type Layout } from "@/lib/api"
import { renderMustache } from "@/lib/api-p3"
import {
  DEFAULT_LAYOUT,
  LayoutFields,
  layoutToWrapper,
  wrapperToLayout,
  SAMPLE_BODY,
  type LayoutModel,
} from "./layout-blocks"
import { Page } from "@/components/page"
import { PageHeader } from "@/components/shared"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { Textarea } from "@/components/ui/textarea"

type Api = ReturnType<typeof serverApi>
function errorToast(err: unknown, fallback: string) {
  toast.error(err instanceof ApiError ? err.message : fallback)
}

type LayoutMode = "editor" | "html"

export function LayoutEditor({
  api,
  org,
  server,
  permalink,
}: {
  api: Api
  org: string
  server: string
  permalink: string | null
}) {
  const router = useRouter()
  const gallery = `/orgs/${org}/servers/${server}/layouts`
  const layouts = useQuery({ queryKey: ["sapi-layouts"], queryFn: api.layouts.list })
  const existing: Layout | undefined = useMemo(
    () => layouts.data?.layouts.find((l) => l.permalink === permalink),
    [layouts.data, permalink],
  )
  const loading = permalink !== null && layouts.isLoading

  return loading ? (
    <p className="text-sm text-muted-foreground">Loading layout…</p>
  ) : permalink !== null && !existing ? (
    <p className="text-sm text-muted-foreground">
      Layout not found.{" "}
      <Link href={gallery} className="text-primary hover:underline">
        Back to layouts
      </Link>
    </p>
  ) : (
    <LayoutForm
      api={api}
      gallery={gallery}
      existing={existing ?? null}
      onSaved={() => {
        layouts.refetch()
        router.push(gallery)
      }}
    />
  )
}

function LayoutForm({
  api,
  gallery,
  existing,
  onSaved,
}: {
  api: Api
  gallery: string
  existing: Layout | null
  onSaved: () => void
}) {
  const [name, setName] = useState(existing?.name ?? "")
  const init = useMemo(() => {
    const parsed = wrapperToLayout(existing?.html_wrapper)
    if (parsed) return { model: parsed, mode: "editor" as LayoutMode, html: existing?.html_wrapper ?? "" }
    if (existing?.html_wrapper) return { model: DEFAULT_LAYOUT, mode: "html" as LayoutMode, html: existing.html_wrapper }
    return { model: DEFAULT_LAYOUT, mode: "editor" as LayoutMode, html: layoutToWrapper(DEFAULT_LAYOUT) }
  }, [existing])
  const [mode, setMode] = useState<LayoutMode>(init.mode)
  const [model, setModel] = useState<LayoutModel>(init.model)
  const [htmlWrapper, setHtmlWrapper] = useState(init.html)
  const [textWrapper, setTextWrapper] = useState(existing?.text_wrapper ?? "")
  const [busy, setBusy] = useState(false)

  function updateModel(next: LayoutModel) {
    setModel(next)
    setHtmlWrapper(layoutToWrapper(next))
  }
  function switchMode(next: LayoutMode) {
    if (next === mode) return
    if (next === "editor") {
      const parsed = wrapperToLayout(htmlWrapper)
      if (parsed) setModel(parsed)
      else toast("This wrapper wasn’t built with the editor. Editing fields replaces it.")
    }
    setMode(next)
  }

  const hasContent = /\{\{\{\s*content\s*\}\}\}|\{\{&\s*content\s*\}\}/.test(htmlWrapper)
  const preview = hasContent
    ? renderMustache(htmlWrapper, { product: "Acme", unsubscribe_url: "#", content: SAMPLE_BODY })
    : ""

  async function save() {
    if (!hasContent) {
      toast.error("The layout must embed the body with {{{ content }}} (raw).")
      return
    }
    setBusy(true)
    try {
      // Ensure the layout exists first — a logo upload needs its permalink.
      let permalink = existing?.permalink ?? null
      if (!permalink) {
        const created = await api.layouts.create({
          name,
          html_wrapper: htmlWrapper,
          ...(textWrapper ? { text_wrapper: textWrapper } : {}),
        })
        permalink = created.layout.permalink
      }
      // A freshly-picked logo is a data: URI; upload it and rebuild the
      // wrapper to reference the served URL so the image arrives in real mail.
      let finalWrapper = htmlWrapper
      if (model.logo.startsWith("data:")) {
        const { url } = await api.layouts.uploadLogo(permalink, model.logo)
        const nextModel = { ...model, logo: url }
        finalWrapper = layoutToWrapper(nextModel)
        setModel(nextModel)
        setHtmlWrapper(finalWrapper)
      }
      await api.layouts.update(permalink, {
        name,
        html_wrapper: finalWrapper,
        text_wrapper: textWrapper,
      })
      toast.success(existing ? "Layout saved" : "Layout created")
      onSaved()
    } catch (err) {
      errorToast(err, "Could not save the layout")
    } finally {
      setBusy(false)
    }
  }

  return (
    <Page
      header={
        <PageHeader
          className="mb-0"
          backHref={gallery}
          backLabel="Layouts"
          title={existing ? existing.name : name || "New layout"}
          action={
            <>
              <Button variant="outline" size="sm" asChild>
                <Link href={gallery}>Cancel</Link>
              </Button>
              <Button size="sm" onClick={save} disabled={busy || !name.trim()}>
                {busy ? "Saving…" : existing ? "Save" : "Create"}
              </Button>
            </>
          }
        />
      }
    >
      <div className="grid gap-4 lg:grid-cols-2">
        {/* Left: config */}
        <div className="grid content-start gap-4">
          <div className="grid gap-1.5">
            <Label>Name / slug</Label>
            <Input
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="Brand"
              disabled={!!existing}
              className="font-mono text-sm"
            />
          </div>

          <Tabs value={mode} onValueChange={(v) => switchMode(v as LayoutMode)}>
            <div className="flex items-center justify-between gap-2">
              <Label>Design</Label>
              <TabsList>
                <TabsTrigger value="editor">Editor</TabsTrigger>
                <TabsTrigger value="html">HTML</TabsTrigger>
              </TabsList>
            </div>

            <TabsContent value="editor" className="mt-2">
              <LayoutFields model={model} onChange={updateModel} />
            </TabsContent>

            <TabsContent value="html" className="mt-2 grid gap-3">
              <div className="grid gap-1.5">
                <Label>HTML wrapper</Label>
                <Textarea
                  rows={16}
                  value={htmlWrapper}
                  onChange={(e) => setHtmlWrapper(e.target.value)}
                  className="font-mono text-xs"
                />
                {!hasContent && (
                  <p className="text-xs text-red-600 dark:text-red-400">
                    The wrapper must embed the body with {"{{{ content }}}"} (raw interpolation).
                  </p>
                )}
              </div>
              <div className="grid gap-1.5">
                <Label>Plain-text wrapper (optional)</Label>
                <Textarea
                  rows={4}
                  value={textWrapper}
                  onChange={(e) => setTextWrapper(e.target.value)}
                  className="font-mono text-xs"
                  placeholder={"{{{ content }}}\n--\nAcme"}
                />
              </div>
            </TabsContent>
          </Tabs>
        </div>

        {/* Right: live preview */}
        <div className="grid content-start gap-1.5">
          <Label>Preview</Label>
          {preview ? (
            <iframe
              title="Layout preview"
              sandbox=""
              srcDoc={preview}
              className="h-[70svh] w-full rounded-md border bg-white"
            />
          ) : (
            <p className="rounded-md border border-dashed p-8 text-center text-sm text-muted-foreground">
              Add {"{{{ content }}}"} to see the preview.
            </p>
          )}
        </div>
      </div>
    </Page>
  )
}
