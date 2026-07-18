"use client"

// A reusable Import lightbox. The caller passes a column definition
// (header + example value per field) and an `onImport` callback; the user
// downloads a CSV template, uploads a filled-in CSV (parsed client-side),
// sees a preview and row count, then imports. The callback does the actual
// per-row API calls (via `runRowImport`) and reports success / failure
// counts, which land in a toast.

import { useRef, useState } from "react"
import { DownloadIcon, UploadIcon } from "lucide-react"
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
import { downloadFile } from "@/lib/api-p2"

/// One importable field: the CSV column `key` (header), a human `label`,
/// an `example` value for the template's sample row, and whether a row is
/// dropped when this field is blank.
export type ImportColumn = {
  key: string
  label: string
  example: string
  required?: boolean
}

/// A parsed row: column key → cell value.
export type ImportRow = Record<string, string>

export type ImportResult = { imported: number; failed: number; errors: string[] }

/// Runs one create call per row, tolerating per-row failures and reporting
/// running progress. The first few error messages are kept for the toast.
export async function runRowImport(
  rows: ImportRow[],
  createOne: (row: ImportRow, index: number) => Promise<unknown>,
  onProgress: (done: number, total: number) => void,
): Promise<ImportResult> {
  let imported = 0
  let failed = 0
  const errors: string[] = []
  for (let i = 0; i < rows.length; i++) {
    try {
      await createOne(rows[i], i)
      imported++
    } catch (err) {
      failed++
      if (errors.length < 5) {
        errors.push(err instanceof Error ? err.message : String(err))
      }
    }
    onProgress(i + 1, rows.length)
  }
  return { imported, failed, errors }
}

const csvEscape = (value: string) =>
  /[",\n]/.test(value) ? `"${value.replaceAll('"', '""')}"` : value

/// A minimal RFC-4180 CSV parser: handles quoted fields, escaped quotes
/// (""), embedded commas and newlines, and both CRLF and LF line endings.
function parseCsv(text: string): string[][] {
  const rows: string[][] = []
  let row: string[] = []
  let field = ""
  let inQuotes = false
  for (let i = 0; i < text.length; i++) {
    const char = text[i]
    if (inQuotes) {
      if (char === '"') {
        if (text[i + 1] === '"') {
          field += '"'
          i++
        } else {
          inQuotes = false
        }
      } else {
        field += char
      }
    } else if (char === '"') {
      inQuotes = true
    } else if (char === ",") {
      row.push(field)
      field = ""
    } else if (char === "\n" || char === "\r") {
      if (char === "\r" && text[i + 1] === "\n") i++
      row.push(field)
      rows.push(row)
      row = []
      field = ""
    } else {
      field += char
    }
  }
  // Trailing field / row (files without a final newline).
  if (field.length > 0 || row.length > 0) {
    row.push(field)
    rows.push(row)
  }
  return rows
}

/// Turns raw CSV text into typed rows. The header row is matched against
/// the known columns (case-insensitive, tolerant of the human label too);
/// unknown columns are ignored, and fully blank or required-field-missing
/// rows are dropped.
function rowsFromCsv(text: string, columns: ImportColumn[]): ImportRow[] {
  const grid = parseCsv(text).filter((r) => r.some((cell) => cell.trim() !== ""))
  if (grid.length < 1) return []
  const header = grid[0].map((h) => h.trim().toLowerCase())
  const indexFor = (column: ImportColumn) => {
    const byKey = header.indexOf(column.key.toLowerCase())
    if (byKey !== -1) return byKey
    return header.indexOf(column.label.toLowerCase())
  }
  const map = columns.map((column) => [column, indexFor(column)] as const)
  const out: ImportRow[] = []
  for (const cells of grid.slice(1)) {
    const record: ImportRow = {}
    for (const [column, index] of map) {
      record[column.key] = index === -1 ? "" : (cells[index] ?? "").trim()
    }
    const hasRequired = columns
      .filter((c) => c.required)
      .every((c) => record[c.key].length > 0)
    const hasAny = Object.values(record).some((v) => v.length > 0)
    if (hasAny && hasRequired) out.push(record)
  }
  return out
}

export function DataImportDialog({
  open,
  onOpenChange,
  title = "Import",
  description,
  templateFilename,
  columns,
  onImport,
  onDone,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  title?: string
  description?: string
  /** Base file name for the downloadable template, without extension. */
  templateFilename: string
  columns: ImportColumn[]
  onImport: (
    rows: ImportRow[],
    onProgress: (done: number, total: number) => void,
  ) => Promise<ImportResult>
  /** Called after a successful import so the caller can refresh its list. */
  onDone?: () => void
}) {
  const inputRef = useRef<HTMLInputElement>(null)
  const [fileName, setFileName] = useState<string | null>(null)
  const [rows, setRows] = useState<ImportRow[]>([])
  const [parseError, setParseError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const [progress, setProgress] = useState<{ done: number; total: number } | null>(null)

  function reset() {
    setFileName(null)
    setRows([])
    setParseError(null)
    setBusy(false)
    setProgress(null)
    if (inputRef.current) inputRef.current.value = ""
  }

  function close(next: boolean) {
    if (!next) reset()
    onOpenChange(next)
  }

  function downloadTemplate() {
    const header = columns.map((c) => csvEscape(c.key)).join(",")
    const example = columns.map((c) => csvEscape(c.example)).join(",")
    downloadFile(`${templateFilename}.csv`, `${header}\n${example}\n`, "text/csv;charset=utf-8")
  }

  async function onFile(file: File) {
    setParseError(null)
    try {
      const text = await file.text()
      const parsed = rowsFromCsv(text, columns)
      setFileName(file.name)
      setRows(parsed)
      if (parsed.length === 0) {
        setParseError("No importable rows found. Check the header row against the template.")
      }
    } catch {
      setParseError("Could not read the file.")
      setRows([])
    }
  }

  async function runImport() {
    setBusy(true)
    setProgress({ done: 0, total: rows.length })
    try {
      const result = await onImport(rows, (done, total) => setProgress({ done, total }))
      if (result.failed === 0) {
        toast.success(`Imported ${result.imported} row${result.imported === 1 ? "" : "s"}`)
      } else {
        toast.error(
          `Imported ${result.imported}, ${result.failed} failed` +
            (result.errors[0] ? `: ${result.errors[0]}` : ""),
        )
      }
      onDone?.()
      close(false)
    } catch {
      toast.error("Import failed")
      setBusy(false)
      setProgress(null)
    }
  }

  const preview = rows.slice(0, 5)
  const pct = progress && progress.total > 0 ? (progress.done / progress.total) * 100 : 0

  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogContent className="max-h-[85vh] overflow-y-auto sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription>
            {description ??
              "Upload a CSV with a header row. Download the template for the exact columns."}
          </DialogDescription>
        </DialogHeader>

        <div className="grid gap-4">
          <div className="flex flex-wrap items-center gap-2">
            <Button variant="outline" size="sm" onClick={downloadTemplate}>
              <DownloadIcon className="size-4" /> Download template
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() => inputRef.current?.click()}
              disabled={busy}
            >
              <UploadIcon className="size-4" /> Choose CSV file
            </Button>
            <input
              ref={inputRef}
              type="file"
              accept=".csv,text/csv"
              className="hidden"
              onChange={(e) => {
                const file = e.target.files?.[0]
                if (file) onFile(file)
              }}
            />
            {fileName && (
              <span className="min-w-0 truncate text-xs text-muted-foreground">{fileName}</span>
            )}
          </div>

          <p className="rounded-md border bg-muted/40 p-2 text-xs text-muted-foreground">
            Format: CSV, UTF-8, one row per record. Columns:{" "}
            {columns.map((c) => c.key).join(", ")}.
          </p>

          {parseError && <p className="text-sm text-destructive">{parseError}</p>}

          {rows.length > 0 && (
            <div className="grid gap-2">
              <Label>
                Preview ({rows.length} row{rows.length === 1 ? "" : "s"})
              </Label>
              <div className="overflow-x-auto rounded-md border">
                <table className="w-full text-left text-xs">
                  <thead className="bg-muted/50">
                    <tr>
                      {columns.map((c) => (
                        <th key={c.key} className="whitespace-nowrap px-2 py-1.5 font-medium">
                          {c.label}
                        </th>
                      ))}
                    </tr>
                  </thead>
                  <tbody>
                    {preview.map((row, index) => (
                      <tr key={index} className="border-t">
                        {columns.map((c) => (
                          <td key={c.key} className="max-w-48 truncate px-2 py-1.5">
                            {row[c.key] || "—"}
                          </td>
                        ))}
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
              {rows.length > preview.length && (
                <p className="text-xs text-muted-foreground">
                  Showing the first {preview.length} of {rows.length} rows.
                </p>
              )}
            </div>
          )}

          {progress && (
            <div className="grid gap-1">
              <div className="h-2 w-full overflow-hidden rounded-full bg-muted">
                <div
                  className="h-full rounded-full bg-primary transition-all"
                  style={{ width: `${pct}%` }}
                />
              </div>
              <p className="text-xs text-muted-foreground">
                Importing {progress.done} of {progress.total}…
              </p>
            </div>
          )}
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={() => close(false)} disabled={busy}>
            Cancel
          </Button>
          <Button onClick={runImport} disabled={busy || rows.length === 0}>
            {busy
              ? "Importing…"
              : `Import${rows.length > 0 ? ` ${rows.length}` : ""}`}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
