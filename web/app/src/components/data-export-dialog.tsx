"use client"

// A reusable Export lightbox. The caller passes a column definition (a
// label + an accessor per field) and the rows the view already holds; the
// user picks which columns to include and a file format (CSV, JSON, or an
// Excel-friendly CSV), and the file is generated client-side and
// downloaded. No server round-trip.

import { useMemo, useState } from "react"
import { DownloadIcon } from "lucide-react"
import { toast } from "sonner"
import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { Label } from "@/components/ui/label"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { downloadFile } from "@/lib/api-p2"

/// One exportable field: a stable `key` (used as the CSV header and JSON
/// property), a human `label` shown in the picker, and an `accessor` that
/// reads the value off a row.
export type ExportColumn<T> = {
  key: string
  label: string
  accessor: (row: T) => unknown
}

type ExportFormat = "csv" | "json" | "excel"

const FORMATS: { value: ExportFormat; label: string }[] = [
  { value: "csv", label: "CSV" },
  { value: "json", label: "JSON" },
  { value: "excel", label: "Excel (.csv)" },
]

/// A CSV cell value as a string, with quoting only where a delimiter,
/// quote or newline would otherwise break the row.
function csvEscape(value: unknown): string {
  const text = stringify(value)
  return /[",\n]/.test(text) ? `"${text.replaceAll('"', '""')}"` : text
}

/// A scalar rendering used for CSV output. Objects fall back to JSON so a
/// nested value never silently becomes "[object Object]".
function stringify(value: unknown): string {
  if (value === null || value === undefined) return ""
  if (typeof value === "string") return value
  if (typeof value === "number" || typeof value === "boolean") return String(value)
  return JSON.stringify(value)
}

export function DataExportDialog<T>({
  open,
  onOpenChange,
  title = "Export",
  description,
  filename,
  columns,
  rows,
  defaultFormat = "csv",
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  title?: string
  description?: string
  /** Base file name, without extension. */
  filename: string
  columns: ExportColumn<T>[]
  rows: T[]
  /** Which format is preselected. JSON suits records with long HTML bodies. */
  defaultFormat?: ExportFormat
}) {
  const [format, setFormat] = useState<ExportFormat>(defaultFormat)
  const [selected, setSelected] = useState<Set<string>>(
    () => new Set(columns.map((c) => c.key)),
  )

  const chosen = useMemo(
    () => columns.filter((c) => selected.has(c.key)),
    [columns, selected],
  )
  const allOn = chosen.length === columns.length
  const canExport = chosen.length > 0 && rows.length > 0

  function toggle(key: string, on: boolean) {
    setSelected((current) => {
      const next = new Set(current)
      if (on) next.add(key)
      else next.delete(key)
      return next
    })
  }

  function toggleAll(on: boolean) {
    setSelected(on ? new Set(columns.map((c) => c.key)) : new Set())
  }

  function runExport() {
    if (format === "json") {
      const data = rows.map((row) => {
        const record: Record<string, unknown> = {}
        for (const column of chosen) record[column.key] = column.accessor(row)
        return record
      })
      downloadFile(`${filename}.json`, JSON.stringify(data, null, 2), "application/json")
    } else {
      const header = chosen.map((c) => csvEscape(c.label))
      const body = rows.map((row) =>
        chosen.map((column) => csvEscape(column.accessor(row))).join(","),
      )
      const csv = [header.join(","), ...body].join("\n") + "\n"
      // A UTF-8 BOM makes Excel read the file as UTF-8 rather than the
      // legacy locale codepage — the only difference from plain CSV.
      const content = format === "excel" ? "﻿" + csv : csv
      downloadFile(`${filename}.csv`, content, "text/csv;charset=utf-8")
    }
    toast.success(`Exported ${rows.length} row${rows.length === 1 ? "" : "s"}`)
    onOpenChange(false)
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[85vh] overflow-y-auto sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription>
            {description ??
              `Download ${rows.length} row${rows.length === 1 ? "" : "s"} as a file.`}
          </DialogDescription>
        </DialogHeader>

        <div className="grid gap-4">
          <div className="grid gap-2">
            <div className="flex items-center justify-between">
              <Label>Columns</Label>
              <button
                type="button"
                className="text-xs text-muted-foreground hover:text-foreground"
                onClick={() => toggleAll(!allOn)}
              >
                {allOn ? "Clear all" : "Select all"}
              </button>
            </div>
            <div className="grid max-h-56 gap-1 overflow-y-auto rounded-md border p-2">
              {columns.map((column) => (
                <label
                  key={column.key}
                  className="flex cursor-pointer items-center gap-2 rounded px-1.5 py-1 text-sm hover:bg-muted/60"
                >
                  <input
                    type="checkbox"
                    className="size-4 accent-primary"
                    checked={selected.has(column.key)}
                    onChange={(e) => toggle(column.key, e.target.checked)}
                  />
                  <span className="min-w-0 flex-1 truncate">{column.label}</span>
                </label>
              ))}
            </div>
          </div>

          <div className="grid gap-2">
            <Label>Format</Label>
            <Select value={format} onValueChange={(v) => setFormat(v as ExportFormat)}>
              <SelectTrigger className="w-48">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {FORMATS.map((f) => (
                  <SelectItem key={f.value} value={f.value}>
                    {f.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button onClick={runExport} disabled={!canExport}>
            <DownloadIcon className="size-4" /> Export
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
