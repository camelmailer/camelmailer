// Structured editor model for a layout (the shared shell around every mail).
// A layout serializes to an `html_wrapper` document that embeds the body via
// {{{ content }}} and carries a marker comment so the visual editor can
// round-trip. The logo is stored as a data URI inside the wrapper — i.e.
// "hard in Postgres" (the layouts.html_wrapper column), no asset service.
"use client"

import { useRef } from "react"
import { ImageIcon, Trash2Icon, UploadIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Textarea } from "@/components/ui/textarea"

export type LayoutModel = {
  logo: string // data URI or served URL; "" to use the brand text
  logoHeight: number
  brand: string // header text when there is no logo (may contain {{ product }})
  // Color scheme — applied across every mail that uses this layout.
  primary: string // header bar, buttons, links
  background: string // page background behind the card
  text: string // body text color
  fontFamily: string
  footer: string // may contain {{ variables }}
}

export const FONT_STACKS: { label: string; value: string }[] = [
  { label: "System sans", value: "-apple-system, Segoe UI, Roboto, Helvetica, Arial, sans-serif" },
  { label: "Helvetica / Arial", value: "Helvetica, Arial, sans-serif" },
  { label: "Georgia (serif)", value: "Georgia, 'Times New Roman', serif" },
  { label: "Verdana", value: "Verdana, Geneva, sans-serif" },
  { label: "Trebuchet", value: "'Trebuchet MS', Helvetica, sans-serif" },
  { label: "Courier (mono)", value: "'Courier New', Courier, monospace" },
]

export const DEFAULT_LAYOUT: LayoutModel = {
  logo: "",
  logoHeight: 28,
  brand: "{{ product }}",
  primary: "#4f46e5",
  background: "#f4f4f5",
  text: "#3f3f46",
  fontFamily: FONT_STACKS[0].value,
  footer: "{{ product }} · 500 Terry Francois Blvd, San Francisco, CA",
}

function esc(v: string): string {
  return v.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;")
}
const escM = (v: string) => esc(v).replace(/\r?\n/g, "<br>")

const MARKER_RE = /<!--cm:layout:([A-Za-z0-9+/=]*)-->/

/** Full html_wrapper document for a layout model (with round-trip marker). */
export function layoutToWrapper(m: LayoutModel): string {
  const header = m.logo
    ? `<img src="${m.logo}" alt="${esc(m.brand)}" style="display:block;height:${m.logoHeight}px;border:0;">`
    : `<span style="color:#ffffff;font-size:18px;font-weight:700;letter-spacing:.3px;">${escM(m.brand)}</span>`
  const marker = `<!--cm:layout:${btoa(encodeURIComponent(JSON.stringify(m)))}-->`
  const font = esc(m.fontFamily)
  return `${marker}
<!doctype html>
<html>
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="color-scheme" content="light">
</head>
<body style="margin:0;padding:0;background:${esc(m.background)};font-family:${font};color:${esc(m.text)};">
  <table role="presentation" width="100%" cellpadding="0" cellspacing="0" style="background:${esc(m.background)};font-family:${font};color:${esc(m.text)};">
    <tr><td align="center" style="padding:32px 16px;">
      <table role="presentation" width="560" cellpadding="0" cellspacing="0" style="max-width:560px;width:100%;">
        <tr><td style="background:${esc(m.primary)};border-radius:12px 12px 0 0;padding:20px 32px;">
          ${header}
        </td></tr>
        <tr><td style="background:#ffffff;border:1px solid #e4e4e7;border-top:none;border-radius:0 0 12px 12px;padding:32px;color:${esc(m.text)};">
{{{ content }}}
        </td></tr>
        <tr><td style="padding:16px 32px;color:#a1a1aa;font-size:12px;line-height:1.5;">
          ${escM(m.footer)}
        </td></tr>
      </table>
    </td></tr>
  </table>
</body>
</html>`
}

/** Parse a layout model back out of a marker-carrying wrapper, or null. */
export function wrapperToLayout(html: string | null | undefined): LayoutModel | null {
  if (!html) return null
  const match = html.match(MARKER_RE)
  if (!match) return null
  try {
    const parsed = JSON.parse(decodeURIComponent(atob(match[1])))
    // Migrate the old single `headerBg` field to the new `primary`.
    if (parsed.headerBg && !parsed.primary) parsed.primary = parsed.headerBg
    return { ...DEFAULT_LAYOUT, ...parsed } as LayoutModel
  } catch {
    return null
  }
}

/** Sample content used to preview a wrapper as a real mail. */
export const SAMPLE_BODY = `<h1 style="margin:0 0 16px;font-size:20px;line-height:1.3;color:#18181b;">Your message heading</h1>
<p style="margin:0 0 16px;font-size:15px;line-height:1.6;color:#3f3f46;">This is how the body of a mail looks inside this layout. Templates fill this area with their own blocks.</p>
<table role="presentation" cellpadding="0" cellspacing="0" style="margin:8px 0 4px;"><tr><td style="border-radius:8px;background:#4f46e5;"><a href="#" style="display:inline-block;padding:12px 24px;font-size:15px;font-weight:600;color:#ffffff;text-decoration:none;border-radius:8px;">A call to action</a></td></tr></table>`

// ---------- editor UI ----------

export function LayoutFields({
  model,
  onChange,
}: {
  model: LayoutModel
  onChange: (next: LayoutModel) => void
}) {
  const fileRef = useRef<HTMLInputElement>(null)
  const set = (patch: Partial<LayoutModel>) => onChange({ ...model, ...patch })

  function onPickLogo(e: React.ChangeEvent<HTMLInputElement>) {
    const file = e.target.files?.[0]
    e.target.value = "" // allow re-picking the same file
    if (!file) return
    const reader = new FileReader()
    reader.onload = () => set({ logo: String(reader.result ?? "") })
    reader.readAsDataURL(file)
  }

  return (
    <div className="grid gap-4">
      {/* logo */}
      <div className="grid gap-1.5">
        <Label>Logo</Label>
        <input
          ref={fileRef}
          type="file"
          accept="image/png,image/jpeg,image/gif,image/svg+xml,image/webp"
          className="hidden"
          onChange={onPickLogo}
        />
        {model.logo ? (
          <div className="flex items-center gap-3 rounded-md border bg-muted/20 p-3">
            {/* eslint-disable-next-line @next/next/no-img-element */}
            <img
              src={model.logo}
              alt="Logo preview"
              className="max-h-12 max-w-[160px] rounded bg-white object-contain p-1"
            />
            <div className="flex items-center gap-2">
              <Button type="button" variant="outline" size="sm" onClick={() => fileRef.current?.click()}>
                <UploadIcon className="size-3.5" /> Replace
              </Button>
              <Button
                type="button"
                variant="ghost"
                size="sm"
                className="text-muted-foreground hover:text-destructive"
                onClick={() => set({ logo: "" })}
              >
                <Trash2Icon className="size-3.5" /> Remove
              </Button>
            </div>
          </div>
        ) : (
          <button
            type="button"
            onClick={() => fileRef.current?.click()}
            className="flex flex-col items-center justify-center gap-1 rounded-md border border-dashed p-6 text-sm text-muted-foreground transition-colors hover:border-primary hover:text-foreground"
          >
            <ImageIcon className="size-5" />
            Upload a logo (PNG, JPG, SVG)
          </button>
        )}
        {model.logo && (
          <div className="flex items-center gap-2">
            <Label className="text-[11px] text-muted-foreground">Height</Label>
            <Input
              type="number"
              min={12}
              max={80}
              value={model.logoHeight}
              onChange={(e) => set({ logoHeight: Number(e.target.value) || 28 })}
              className="w-24 text-sm"
            />
            <span className="text-xs text-muted-foreground">px</span>
          </div>
        )}
      </div>

      {/* brand text (fallback when no logo) */}
      {!model.logo && (
        <div className="grid gap-1.5">
          <Label>Brand text</Label>
          <Input
            value={model.brand}
            onChange={(e) => set({ brand: e.target.value })}
            placeholder="{{ product }}"
            className="text-sm"
          />
          <p className="text-xs text-muted-foreground">
            Shown in the header when no logo is set. Supports {"{{ variables }}"}.
          </p>
        </div>
      )}

      {/* color scheme */}
      <div className="grid gap-2">
        <Label>Color scheme</Label>
        <p className="-mt-1 text-xs text-muted-foreground">
          Applied to every mail that uses this layout.
        </p>
        <div className="grid gap-2 sm:grid-cols-3">
          <ColorField label="Primary" value={model.primary} onChange={(v) => set({ primary: v })} />
          <ColorField label="Background" value={model.background} onChange={(v) => set({ background: v })} />
          <ColorField label="Text" value={model.text} onChange={(v) => set({ text: v })} />
        </div>
      </div>

      {/* font family */}
      <div className="grid gap-1.5">
        <Label>Font family</Label>
        <select
          value={FONT_STACKS.some((f) => f.value === model.fontFamily) ? model.fontFamily : ""}
          onChange={(e) => set({ fontFamily: e.target.value })}
          className="h-9 rounded-md border bg-background px-3 text-sm"
          style={{ fontFamily: model.fontFamily }}
        >
          {FONT_STACKS.map((f) => (
            <option key={f.label} value={f.value} style={{ fontFamily: f.value }}>
              {f.label}
            </option>
          ))}
        </select>
      </div>

      {/* footer */}
      <div className="grid gap-1.5">
        <Label>Footer</Label>
        <Textarea
          rows={3}
          value={model.footer}
          onChange={(e) => set({ footer: e.target.value })}
          placeholder="Company · address · unsubscribe"
          className="text-sm"
        />
        <p className="text-xs text-muted-foreground">
          Shared footer for every mail. Supports {"{{ variables }}"} (e.g. {"{{ unsubscribe_url }}"}).
        </p>
      </div>
    </div>
  )
}

function ColorField({
  label,
  value,
  onChange,
}: {
  label: string
  value: string
  onChange: (value: string) => void
}) {
  return (
    <div className="grid gap-1">
      <Label className="text-[11px] text-muted-foreground">{label}</Label>
      <div className="flex items-center gap-1.5">
        <input
          type="color"
          value={value}
          onChange={(e) => onChange(e.target.value)}
          className="h-9 w-9 shrink-0 cursor-pointer rounded-md border bg-background p-1"
          aria-label={`${label} color`}
        />
        <Input value={value} onChange={(e) => onChange(e.target.value)} className="font-mono text-xs" />
      </div>
    </div>
  )
}
