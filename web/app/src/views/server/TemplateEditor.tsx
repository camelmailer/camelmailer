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
import { ArrowLeftIcon, InfoIcon } from "lucide-react"
import { toast } from "sonner"
import { ApiError, serverApi, type Template } from "@/lib/api"
import { renderMustache, sampleModel, extractVariables } from "@/lib/api-p3"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
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
  const gallery = `/orgs/${org}/servers/${server}/messaging/templates`

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
  const [htmlBody, setHtmlBody] = useState(existing?.html_body ?? "")
  const [textBody, setTextBody] = useState(existing?.text_body ?? "")
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
  const renderedHtml = useMemo(() => renderMustache(htmlBody, model), [htmlBody, model])
  const renderedText = useMemo(() => renderMustache(textBody, model), [textBody, model])

  async function save() {
    setBusy(true)
    try {
      if (existing) {
        await api.templates.update(existing.permalink, {
          name,
          subject,
          html_body: htmlBody,
          text_body: textBody,
        })
      } else {
        await api.templates.create({
          name,
          ...(subject ? { subject } : {}),
          ...(htmlBody ? { html_body: htmlBody } : {}),
          ...(textBody ? { text_body: textBody } : {}),
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
    <div className="space-y-4">
      {/* Focus header: breadcrumb + primary CTA */}
      <div className="flex flex-wrap items-center justify-between gap-2 border-b pb-3">
        <div className="flex items-center gap-2 text-sm text-muted-foreground">
          <Button variant="ghost" size="icon" asChild className="size-7">
            <Link href={gallery} aria-label="Back to templates">
              <ArrowLeftIcon className="size-4" />
            </Link>
          </Button>
          <Link href={gallery} className="hover:underline">
            Templates
          </Link>
          <span>/</span>
          <span className="font-medium text-foreground">
            {existing ? existing.name : name || "New template"}
          </span>
          {existing?.archived && <Badge variant="secondary">draft</Badge>}
        </div>
        <div className="flex items-center gap-2">
          <Button variant="outline" size="sm" asChild>
            <Link href={gallery}>Cancel</Link>
          </Button>
          <Button size="sm" onClick={save} disabled={busy || !name.trim()}>
            {busy ? "Saving…" : existing ? "Save" : "Publish"}
          </Button>
        </div>
      </div>

      <div className="grid gap-4 lg:grid-cols-2">
        {/* Left: code */}
        <div className="grid content-start gap-4">
          <div className="grid grid-cols-2 gap-2">
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
          </div>
          <div className="grid gap-1.5">
            <Label>HTML body</Label>
            <Textarea
              rows={14}
              value={htmlBody}
              onChange={(e) => setHtmlBody(e.target.value)}
              className="font-mono text-xs"
              placeholder="<h1>Hi {{ name }}</h1>"
            />
          </div>
          <div className="grid gap-1.5">
            <Label>Plain-text body</Label>
            <Textarea
              rows={6}
              value={textBody}
              onChange={(e) => setTextBody(e.target.value)}
              className="font-mono text-xs"
              placeholder="Hi {{ name }}"
            />
          </div>
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

        {/* Right: envelope + live preview */}
        <div className="grid content-start gap-3">
          <div className="grid grid-cols-2 gap-2">
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
                  className="h-[60svh] w-full rounded-md border bg-white"
                />
              ) : (
                <p className="rounded-md border border-dashed p-8 text-center text-sm text-muted-foreground">
                  Add an HTML body to see the preview.
                </p>
              )}
            </TabsContent>

            <TabsContent value="text" className="mt-2">
              <pre className="h-[60svh] overflow-auto whitespace-pre-wrap rounded-md border bg-muted p-3 text-xs">
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
  )
}
