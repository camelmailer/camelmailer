"use client"

// The focus-mode split template editor (masterplan §4.7): code on the
// left (subject / HTML / text, monospace), a live preview on the right
// driven by an editable test model, envelope fields above the preview and
// the Save/Publish CTA in the focus header. The preview renders the
// Mustache subset client-side so *unsaved* edits show instantly.

import { useMemo, useState } from "react"
import Link from "next/link"
import { useRouter } from "next/navigation"
import { useQuery } from "@tanstack/react-query"
import { InfoIcon } from "lucide-react"
import { toast } from "sonner"
import { ApiError, serverApi, type Layout, type Template } from "@/lib/api"
import { renderMustache, sampleModel, extractVariables } from "@/lib/api-p3"
import {
  BlockEditor,
  blocksToHtml,
  htmlToBlocks,
  STARTER_BLOCKS,
  type Block,
} from "./template-blocks"
import { Page } from "@/components/page"
import { PageHeader } from "@/components/shared"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { Textarea } from "@/components/ui/textarea"

type Api = ReturnType<typeof serverApi>

function errorToast(err: unknown, fallback: string) {
  toast.error(err instanceof ApiError ? err.message : fallback)
}

export function TemplateEditor({
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
  const gallery = `/orgs/${org}/servers/${server}/templates`

  // Existing template (list + find — the server API has no single-get).
  const templates = useQuery({ queryKey: ["sapi-templates"], queryFn: api.templates.list })
  const existing: Template | undefined = useMemo(
    () => templates.data?.templates.find((t) => t.permalink === permalink),
    [templates.data, permalink],
  )
  const loading = permalink !== null && templates.isLoading

  return loading ? (
    <p className="text-sm text-muted-foreground">Loading template…</p>
  ) : permalink !== null && !existing ? (
    <p className="text-sm text-muted-foreground">
      Template not found.{" "}
      <Link href={gallery} className="text-primary hover:underline">
        Back to templates
      </Link>
    </p>
  ) : (
    <EditorForm
      api={api}
      gallery={gallery}
      existing={existing ?? null}
      onSaved={() => {
        templates.refetch()
        router.push(gallery)
      }}
    />
  )
}

type EditorMode = "editor" | "html" | "text"

function EditorForm({
  api,
  gallery,
  existing,
  onSaved,
}: {
  api: Api
  gallery: string
  existing: Template | null
  onSaved: () => void
}) {
  const [name, setName] = useState(existing?.name ?? "")
  const [subject, setSubject] = useState(existing?.subject ?? "")
  // Authoring mode + block model. A block-authored template carries a marker
  // comment we can parse back into blocks (→ visual "editor"); arbitrary HTML
  // has none and opens in raw "html"; a brand-new template starts visual.
  const init = useMemo(() => {
    const parsed = htmlToBlocks(existing?.html_body)
    if (parsed) return { blocks: parsed, mode: "editor" as EditorMode, html: existing?.html_body ?? "" }
    if (existing?.html_body) return { blocks: [] as Block[], mode: "html" as EditorMode, html: existing.html_body }
    const starter = STARTER_BLOCKS()
    return { blocks: starter, mode: "editor" as EditorMode, html: blocksToHtml(starter) }
  }, [existing])
  const [mode, setMode] = useState<EditorMode>(init.mode)
  const [blocks, setBlocks] = useState<Block[]>(init.blocks)
  const [htmlBody, setHtmlBody] = useState(init.html)
  const [textBody, setTextBody] = useState(existing?.text_body ?? "")

  // Editing blocks keeps the saved/previewed HTML in sync (no effect needed).
  function updateBlocks(next: Block[]) {
    setBlocks(next)
    setHtmlBody(blocksToHtml(next))
  }
  function switchMode(next: EditorMode) {
    if (next === mode) return
    if (next === "editor") {
      const parsed = htmlToBlocks(htmlBody)
      if (parsed) {
        setBlocks(parsed)
      } else if (blocks.length > 0) {
        toast("Switching to the editor — block edits will replace the current HTML.")
      } else {
        toast("This HTML wasn’t built with the editor. Adding a block replaces it.")
      }
    }
    setMode(next)
  }
  // Layout: "" = none; null = untouched (falls back to the template's
  // stored layout once the layouts list has loaded).
  const layouts = useQuery({ queryKey: ["sapi-layouts"], queryFn: api.layouts.list })
  const [layoutChoice, setLayoutChoice] = useState<string | null>(null)
  const layoutList: Layout[] = useMemo(() => layouts.data?.layouts ?? [], [layouts.data])
  const layoutPermalink =
    layoutChoice ??
    (existing
      ? existing.layout_id
        ? (layoutList.find((l) => l.id === existing.layout_id)?.permalink ?? "")
        : ""
      : // New templates default to the first (branded) layout so the block
        // editor's content-only body previews inside the shared shell.
        (layoutList[0]?.permalink ?? ""))
  const activeLayout = layoutList.find((l) => l.permalink === layoutPermalink) ?? null
  // Envelope fields — preview-only (not persisted; the API has no From /
  // preview-text on a template).
  const [from, setFrom] = useState("hello@yourdomain.com")
  const [previewText, setPreviewText] = useState("")
  const [modelText, setModelText] = useState(() =>
    JSON.stringify(sampleModel(existing?.subject, existing?.html_body, existing?.text_body), null, 2),
  )
  const [busy, setBusy] = useState(false)

  const variables = useMemo(
    () => extractVariables(subject, htmlBody, textBody),
    [subject, htmlBody, textBody],
  )

  const { model, modelError } = useMemo(() => {
    try {
      return { model: JSON.parse(modelText || "{}") as unknown, modelError: null as string | null }
    } catch {
      return { model: {}, modelError: "Test model is not valid JSON" }
    }
  }, [modelText])

  const renderedSubject = useMemo(() => renderMustache(subject, model), [subject, model])
  // The preview wraps in the chosen layout exactly like the server does at
  // send time: the wrapper sees the model plus the rendered body as
  // `content`.
  const renderedHtml = useMemo(() => {
    const body = renderMustache(htmlBody, model)
    if (!body || !activeLayout) return body
    const scoped = { ...(typeof model === "object" && model !== null ? model : {}), content: body }
    return renderMustache(activeLayout.html_wrapper, scoped)
  }, [htmlBody, model, activeLayout])
  const renderedText = useMemo(() => {
    const body = renderMustache(textBody, model)
    if (!body || !activeLayout?.text_wrapper) return body
    const scoped = { ...(typeof model === "object" && model !== null ? model : {}), content: body }
    return renderMustache(activeLayout.text_wrapper, scoped)
  }, [textBody, model, activeLayout])

  async function save() {
    setBusy(true)
    try {
      if (existing) {
        await api.templates.update(existing.permalink, {
          name,
          subject,
          html_body: htmlBody,
          text_body: textBody,
          layout: layoutPermalink,
        })
      } else {
        await api.templates.create({
          name,
          ...(subject ? { subject } : {}),
          ...(htmlBody ? { html_body: htmlBody } : {}),
          ...(textBody ? { text_body: textBody } : {}),
          ...(layoutPermalink ? { layout: layoutPermalink } : {}),
        })
      }
      toast.success(existing ? "Template saved" : "Template published")
      onSaved()
    } catch (err) {
      errorToast(err, "Could not save the template")
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
          backLabel="Templates"
          title={
            <>
              {existing ? existing.name : name || "New template"}
              {existing?.archived && (
                <Badge variant="secondary" className="ml-2 align-middle">
                  draft
                </Badge>
              )}
            </>
          }
          action={
            <>
              <Button variant="outline" size="sm" asChild>
                <Link href={gallery}>Cancel</Link>
              </Button>
              <Button size="sm" onClick={save} disabled={busy || !name.trim()}>
                {busy ? "Saving…" : existing ? "Save" : "Publish"}
              </Button>
            </>
          }
        />
      }
    >
      <div className="grid gap-5">
        {/* Meta: identity + envelope, full width above the fold */}
        <div className="grid gap-4">
          <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
            <div className="grid gap-1.5">
              <Label>Name / slug</Label>
              <Input
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="welcome"
                disabled={!!existing}
                className="font-mono text-sm"
              />
            </div>
            <div className="grid gap-1.5">
              <Label>Subject</Label>
              <Input
                value={subject}
                onChange={(e) => setSubject(e.target.value)}
                placeholder="Hello {{ name }}"
                className="font-mono text-sm"
              />
            </div>
            <div className="grid gap-1.5">
              <Label>From (preview)</Label>
              <Input value={from} onChange={(e) => setFrom(e.target.value)} className="text-sm" />
            </div>
            <div className="grid gap-1.5">
              <Label>Preview text</Label>
              <Input
                value={previewText}
                onChange={(e) => setPreviewText(e.target.value)}
                placeholder="Inbox preview snippet"
                className="text-sm"
              />
            </div>
          </div>

          <div className="grid items-start gap-4 sm:grid-cols-2">
            {layoutList.length > 0 && (
              <div className="grid gap-1.5">
                <Label>Layout</Label>
                <Select
                  value={layoutPermalink === "" ? "none" : layoutPermalink}
                  onValueChange={(value) => setLayoutChoice(value === "none" ? "" : value)}
                >
                  <SelectTrigger className="w-full max-w-64">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="none">No layout</SelectItem>
                    {layoutList.map((layout) => (
                      <SelectItem key={layout.id} value={layout.permalink}>
                        {layout.name}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <p className="text-xs text-muted-foreground">
                  The layout wraps the rendered body with your shared header and footer.
                </p>
              </div>
            )}
            {variables.length > 0 && (
              <div className="rounded-md border bg-muted/30 p-3">
                <p className="mb-1.5 flex items-center gap-1.5 text-xs font-medium text-muted-foreground">
                  <InfoIcon className="size-3.5" />
                  Variables: fill them in the test model to preview
                </p>
                <div className="flex flex-wrap gap-1">
                  {variables.map((v) => (
                    <Badge key={v} variant="outline" className="font-mono text-[11px]">
                      {`{{ ${v} }}`}
                    </Badge>
                  ))}
                </div>
              </div>
            )}
          </div>
        </div>

        {/* Horizontal divider between the meta fields and the workspace */}
        <div className="border-t" />

        {/* Content editor (left) and live preview (right), split vertically */}
        <div className="grid gap-6 lg:grid-cols-2">
          <div className="grid content-start gap-4 lg:border-r lg:pr-6">
            <Tabs value={mode} onValueChange={(v) => switchMode(v as EditorMode)}>
            <div className="flex items-center justify-between gap-2">
              <Label>Content</Label>
              <TabsList>
                <TabsTrigger value="editor">Editor</TabsTrigger>
                <TabsTrigger value="html">HTML</TabsTrigger>
                <TabsTrigger value="text">Plain Text</TabsTrigger>
              </TabsList>
            </div>

            <TabsContent value="editor" className="mt-2">
              <BlockEditor blocks={blocks} onChange={updateBlocks} />
            </TabsContent>

            <TabsContent value="html" className="mt-2 grid gap-1.5">
              <Textarea
                rows={18}
                value={htmlBody}
                onChange={(e) => setHtmlBody(e.target.value)}
                className="font-mono text-xs"
                placeholder="<h1>Hi {{ name }}</h1>"
              />
              <p className="text-xs text-muted-foreground">
                Expert mode: write raw, email-safe HTML. Mustache-style{" "}
                <code className="font-mono">{"{{ variables }}"}</code> are filled from the test model.
              </p>
            </TabsContent>

            <TabsContent value="text" className="mt-2 grid gap-1.5">
              <Textarea
                rows={12}
                value={textBody}
                onChange={(e) => setTextBody(e.target.value)}
                className="font-mono text-xs"
                placeholder="Hi {{ name }}"
              />
              <p className="text-xs text-muted-foreground">
                Plain-text alternative, shown by clients that do not render HTML.
              </p>
            </TabsContent>
          </Tabs>
        </div>

        {/* Right: live preview — sticky so it stays in view and never grows
            past the viewport while the editor column scrolls. */}
        <div className="grid content-start gap-3 lg:sticky lg:top-4 lg:self-start">
          <Tabs defaultValue="html">
            <div className="flex items-center justify-between gap-2">
              <TabsList>
                <TabsTrigger value="html">Preview</TabsTrigger>
                <TabsTrigger value="text">Plain text</TabsTrigger>
                <TabsTrigger value="model">Test model</TabsTrigger>
              </TabsList>
              <span className="truncate text-xs text-muted-foreground">
                {from} · <span className="font-medium text-foreground">{renderedSubject || "—"}</span>
              </span>
            </div>

            <TabsContent value="html" className="mt-2">
              {previewText && (
                <p className="mb-1 truncate text-xs text-muted-foreground">{previewText}</p>
              )}
              {renderedHtml ? (
                <iframe
                  title="Template preview"
                  sandbox=""
                  srcDoc={renderedHtml}
                  className="h-[clamp(24rem,calc(100svh-14rem),40rem)] w-full rounded-md border bg-white"
                />
              ) : (
                <p className="rounded-md border border-dashed p-8 text-center text-sm text-muted-foreground">
                  Add an HTML body to see the preview.
                </p>
              )}
            </TabsContent>

            <TabsContent value="text" className="mt-2">
              <pre className="h-[clamp(24rem,calc(100svh-14rem),40rem)] overflow-auto whitespace-pre-wrap rounded-md border bg-muted p-3 text-xs">
                {renderedText || "No plain-text body."}
              </pre>
            </TabsContent>

            <TabsContent value="model" className="mt-2">
              <Label className="mb-1.5 block">Test model (JSON)</Label>
              <Textarea
                rows={20}
                value={modelText}
                onChange={(e) => setModelText(e.target.value)}
                className="font-mono text-xs"
              />
              {modelError ? (
                <p className="mt-1 text-xs text-red-600 dark:text-red-400">{modelError}</p>
              ) : (
                <p className="mt-1 text-xs text-muted-foreground">
                  Edit the values. The preview updates as you type.
                </p>
              )}
            </TabsContent>
          </Tabs>
        </div>
      </div>
      </div>
    </Page>
  )
}
