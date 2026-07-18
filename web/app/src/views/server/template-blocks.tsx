// A lightweight block-based email builder used by the template editor's
// "Editor" mode. Blocks serialize to email-safe HTML in the same visual
// language as the bundled library templates (560px card, muted body text,
// table-based buttons). The generated HTML carries an invisible marker
// comment holding the block list, so re-opening a block-authored template
// rebuilds the visual editor exactly; hand-written HTML has no marker and
// opens in the raw "HTML" mode instead.
"use client"

import { useRef, useState } from "react"
import {
  ChevronDownIcon,
  ChevronUpIcon,
  GripVerticalIcon,
  Heading1Icon,
  Heading2Icon,
  ImageIcon,
  LinkIcon,
  ListIcon,
  MinusIcon,
  MoveVerticalIcon,
  PilcrowIcon,
  Trash2Icon,
  TypeIcon,
} from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Textarea } from "@/components/ui/textarea"
import { cn } from "@/lib/utils"

// ---------- model ----------

export type Block =
  | { id: string; type: "heading"; text: string }
  | { id: string; type: "subheading"; text: string }
  | { id: string; type: "text"; text: string }
  | { id: string; type: "button"; label: string; url: string; color: string }
  | { id: string; type: "image"; src: string; alt: string; href: string }
  | { id: string; type: "list"; items: string }
  | { id: string; type: "divider" }
  | { id: string; type: "spacer"; size: number }
  | { id: string; type: "footer"; text: string }

type BlockType = Block["type"]

const newId = () =>
  typeof crypto !== "undefined" && "randomUUID" in crypto
    ? crypto.randomUUID()
    : `b_${Date.now()}_${Math.floor(Math.random() * 1e6)}`

function makeBlock(type: BlockType): Block {
  switch (type) {
    case "heading":
      return { id: newId(), type, text: "Welcome aboard, {{ name }}" }
    case "subheading":
      return { id: newId(), type, text: "A quick section title" }
    case "text":
      return {
        id: newId(),
        type,
        text: "Write your message here. You can use {{ variables }} that get filled in on every send.",
      }
    case "button":
      return { id: newId(), type, label: "Open your dashboard", url: "https://example.com/action", color: "#4f46e5" }
    case "image":
      return { id: newId(), type, src: "https://placehold.co/560x200", alt: "", href: "" }
    case "list":
      return { id: newId(), type, items: "First point\nSecond point\nThird point" }
    case "divider":
      return { id: newId(), type }
    case "spacer":
      return { id: newId(), type, size: 24 }
    case "footer":
      return {
        id: newId(),
        type,
        text: "{{ product }} · 500 Terry Francois Blvd, San Francisco, CA · Unsubscribe: {{ unsubscribe_url }}",
      }
  }
}

export const BLOCK_PALETTE: { type: BlockType; label: string; icon: typeof TypeIcon }[] = [
  { type: "heading", label: "Heading", icon: Heading1Icon },
  { type: "subheading", label: "Subheading", icon: Heading2Icon },
  { type: "text", label: "Text", icon: PilcrowIcon },
  { type: "button", label: "Button", icon: LinkIcon },
  { type: "image", label: "Image", icon: ImageIcon },
  { type: "list", label: "List", icon: ListIcon },
  { type: "divider", label: "Divider", icon: MinusIcon },
  { type: "spacer", label: "Spacer", icon: MoveVerticalIcon },
  { type: "footer", label: "Footer", icon: TypeIcon },
]

const BLOCK_LABEL: Record<BlockType, string> = Object.fromEntries(
  BLOCK_PALETTE.map((p) => [p.type, p.label]),
) as Record<BlockType, string>

// ---------- serialization ----------

function esc(value: string): string {
  return value.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;")
}
// Keep {{ mustache }} placeholders intact but escape everything else, and turn
// newlines into <br> for multi-line text.
const escMultiline = (value: string) => esc(value).replace(/\r?\n/g, "<br>")

function blockToHtml(block: Block): string {
  switch (block.type) {
    case "heading":
      return `<h1 style="margin:0 0 16px;font-size:20px;line-height:1.3;color:#18181b;">${escMultiline(block.text)}</h1>`
    case "subheading":
      return `<h3 style="margin:24px 0 8px;font-size:16px;line-height:1.4;color:#18181b;">${escMultiline(block.text)}</h3>`
    case "text":
      return `<p style="margin:0 0 16px;font-size:15px;line-height:1.6;color:#3f3f46;">${escMultiline(block.text)}</p>`
    case "button":
      return `<table role="presentation" cellpadding="0" cellspacing="0" style="margin:8px 0 20px;"><tr><td style="border-radius:8px;background:${esc(block.color)};"><a href="${esc(block.url)}" style="display:inline-block;padding:12px 24px;font-size:15px;font-weight:600;color:#ffffff;text-decoration:none;border-radius:8px;">${esc(block.label)}</a></td></tr></table>`
    case "image": {
      const img = `<img src="${esc(block.src)}" alt="${esc(block.alt)}" style="display:block;width:100%;max-width:100%;border-radius:8px;margin:0 0 16px;">`
      return block.href ? `<a href="${esc(block.href)}">${img}</a>` : img
    }
    case "list": {
      const items = block.items
        .split(/\r?\n/)
        .map((i) => i.trim())
        .filter(Boolean)
        .map((i) => `<li style="margin:0 0 6px;">${esc(i)}</li>`)
        .join("")
      return `<ul style="margin:0 0 16px;padding-left:20px;font-size:15px;line-height:1.6;color:#3f3f46;">${items}</ul>`
    }
    case "divider":
      return `<hr style="border:none;border-top:1px solid #e4e4e7;margin:24px 0;">`
    case "spacer":
      return `<div style="height:${block.size}px;line-height:${block.size}px;font-size:0;">&nbsp;</div>`
    case "footer":
      return `<p style="margin:16px 0 0;font-size:12px;line-height:1.5;color:#a1a1aa;">${escMultiline(block.text)}</p>`
  }
}

const MARKER_RE = /<!--cm:blocks:([A-Za-z0-9+/=]*)-->/

function encodeBlocks(blocks: Block[]): string {
  // base64 of the JSON so the payload never contains "--" (which would close
  // the HTML comment). encodeURIComponent keeps unicode safe through btoa.
  return btoa(encodeURIComponent(JSON.stringify(blocks)))
}

/**
 * The template body for the given blocks: an invisible marker comment (so the
 * editor can round-trip) followed by the stacked block HTML. This is *content
 * only* — the branded shell (header, card, footer) comes from the template's
 * layout, which wraps this body via `{{{ content }}}` at send and preview time.
 */
export function blocksToHtml(blocks: Block[]): string {
  const body = blocks.map(blockToHtml).join("\n")
  const marker = `<!--cm:blocks:${encodeBlocks(blocks)}-->`
  return `${marker}\n${body}`
}

/** Parse blocks back out of a block-authored HTML document, or null. */
export function htmlToBlocks(html: string | null | undefined): Block[] | null {
  if (!html) return null
  const match = html.match(MARKER_RE)
  if (!match) return null
  try {
    const json = decodeURIComponent(atob(match[1]))
    const parsed = JSON.parse(json)
    if (!Array.isArray(parsed)) return null
    return parsed as Block[]
  } catch {
    return null
  }
}

export const STARTER_BLOCKS = (): Block[] => [
  makeBlock("heading"),
  makeBlock("text"),
  makeBlock("button"),
]

// ---------- editor UI ----------

export function BlockEditor({
  blocks,
  onChange,
}: {
  blocks: Block[]
  onChange: (next: Block[]) => void
}) {
  const dragIndex = useRef<number | null>(null)
  const [dragOver, setDragOver] = useState<number | null>(null)

  function update(id: string, patch: Partial<Block>) {
    onChange(blocks.map((b) => (b.id === id ? ({ ...b, ...patch } as Block) : b)))
  }
  function remove(id: string) {
    onChange(blocks.filter((b) => b.id !== id))
  }
  function add(type: BlockType) {
    onChange([...blocks, makeBlock(type)])
  }
  function move(from: number, to: number) {
    if (to < 0 || to >= blocks.length || from === to) return
    const next = blocks.slice()
    const [item] = next.splice(from, 1)
    next.splice(to, 0, item)
    onChange(next)
  }

  return (
    <div className="grid gap-3">
      {/* palette */}
      <div className="flex flex-wrap gap-1.5 rounded-md border bg-muted/30 p-2">
        {BLOCK_PALETTE.map(({ type, label, icon: Icon }) => (
          <Button
            key={type}
            type="button"
            variant="outline"
            size="sm"
            className="h-8 gap-1.5 bg-background"
            onClick={() => add(type)}
          >
            <Icon className="size-3.5" />
            {label}
          </Button>
        ))}
      </div>

      {/* canvas */}
      {blocks.length === 0 ? (
        <div className="rounded-md border border-dashed p-8 text-center text-sm text-muted-foreground">
          Add a block above to start building. Drag blocks by their handle to reorder.
        </div>
      ) : (
        <div className="grid gap-2">
          {blocks.map((block, index) => (
            <div
              key={block.id}
              draggable
              onDragStart={(e) => {
                dragIndex.current = index
                e.dataTransfer.effectAllowed = "move"
              }}
              onDragOver={(e) => {
                e.preventDefault()
                if (dragOver !== index) setDragOver(index)
              }}
              onDragEnd={() => {
                dragIndex.current = null
                setDragOver(null)
              }}
              onDrop={(e) => {
                e.preventDefault()
                if (dragIndex.current !== null) move(dragIndex.current, index)
                dragIndex.current = null
                setDragOver(null)
              }}
              className={cn(
                "rounded-md border bg-card p-3 transition-colors",
                dragOver === index && "border-primary ring-1 ring-primary",
              )}
            >
              <div className="mb-2 flex items-center gap-2">
                <span
                  className="flex cursor-grab items-center text-muted-foreground active:cursor-grabbing"
                  aria-hidden
                >
                  <GripVerticalIcon className="size-4" />
                </span>
                <span className="text-xs font-medium text-muted-foreground">
                  {BLOCK_LABEL[block.type]}
                </span>
                <div className="ml-auto flex items-center gap-0.5">
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon"
                    className="size-7"
                    disabled={index === 0}
                    onClick={() => move(index, index - 1)}
                    aria-label="Move up"
                  >
                    <ChevronUpIcon className="size-3.5" />
                  </Button>
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon"
                    className="size-7"
                    disabled={index === blocks.length - 1}
                    onClick={() => move(index, index + 1)}
                    aria-label="Move down"
                  >
                    <ChevronDownIcon className="size-3.5" />
                  </Button>
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon"
                    className="size-7 text-muted-foreground hover:text-destructive"
                    onClick={() => remove(block.id)}
                    aria-label="Delete block"
                  >
                    <Trash2Icon className="size-3.5" />
                  </Button>
                </div>
              </div>
              <BlockFields block={block} update={update} />
            </div>
          ))}
        </div>
      )}
    </div>
  )
}

function BlockFields({
  block,
  update,
}: {
  block: Block
  update: (id: string, patch: Partial<Block>) => void
}) {
  switch (block.type) {
    case "heading":
    case "subheading":
      return (
        <Input
          value={block.text}
          onChange={(e) => update(block.id, { text: e.target.value })}
          placeholder="Heading text"
          className="text-sm"
        />
      )
    case "text":
    case "footer":
      return (
        <Textarea
          rows={block.type === "footer" ? 2 : 3}
          value={block.text}
          onChange={(e) => update(block.id, { text: e.target.value })}
          placeholder="Text… (supports {{ variables }})"
          className="text-sm"
        />
      )
    case "button":
      return (
        <div className="grid gap-2 sm:grid-cols-[1fr_1fr_auto]">
          <div className="grid gap-1">
            <Label className="text-[11px] text-muted-foreground">Label</Label>
            <Input value={block.label} onChange={(e) => update(block.id, { label: e.target.value })} className="text-sm" />
          </div>
          <div className="grid gap-1">
            <Label className="text-[11px] text-muted-foreground">Link URL</Label>
            <Input value={block.url} onChange={(e) => update(block.id, { url: e.target.value })} className="font-mono text-xs" />
          </div>
          <div className="grid gap-1">
            <Label className="text-[11px] text-muted-foreground">Color</Label>
            <input
              type="color"
              value={block.color}
              onChange={(e) => update(block.id, { color: e.target.value })}
              className="h-9 w-12 cursor-pointer rounded-md border bg-background p-1"
              aria-label="Button color"
            />
          </div>
        </div>
      )
    case "image":
      return (
        <div className="grid gap-2 sm:grid-cols-2">
          <div className="grid gap-1">
            <Label className="text-[11px] text-muted-foreground">Image URL</Label>
            <Input value={block.src} onChange={(e) => update(block.id, { src: e.target.value })} className="font-mono text-xs" />
          </div>
          <div className="grid gap-1">
            <Label className="text-[11px] text-muted-foreground">Link URL (optional)</Label>
            <Input value={block.href} onChange={(e) => update(block.id, { href: e.target.value })} className="font-mono text-xs" />
          </div>
          <div className="grid gap-1 sm:col-span-2">
            <Label className="text-[11px] text-muted-foreground">Alt text</Label>
            <Input value={block.alt} onChange={(e) => update(block.id, { alt: e.target.value })} className="text-sm" />
          </div>
        </div>
      )
    case "list":
      return (
        <Textarea
          rows={3}
          value={block.items}
          onChange={(e) => update(block.id, { items: e.target.value })}
          placeholder="One item per line"
          className="text-sm"
        />
      )
    case "spacer":
      return (
        <div className="flex items-center gap-2">
          <Label className="text-[11px] text-muted-foreground">Height</Label>
          <Input
            type="number"
            min={4}
            max={120}
            value={block.size}
            onChange={(e) => update(block.id, { size: Number(e.target.value) || 0 })}
            className="w-24 text-sm"
          />
          <span className="text-xs text-muted-foreground">px</span>
        </div>
      )
    case "divider":
      return <p className="text-xs text-muted-foreground">A horizontal rule.</p>
  }
}
