"use client"

// A reusable, scanning-optimized data table (shadcn + TanStack Table),
// following the datatable-ux rules: prominent search, optional Filter
// dropdowns for a primary dimension, sortable headers with a visible
// affordance, a distinct tinted header, roomy hairline-divided rows, a
// one-scale text-sm chrome, normal-case headers, left-aligned data
// columns (numbers included) with only the trailing actions column
// right-aligned, real loading/empty states and a client-side pagination
// footer (rows-per-page + prev/next). Every dashboard table reuses this.

import * as React from "react"
import {
  type ColumnDef,
  type ColumnFiltersState,
  type Header,
  type SortingState,
  flexRender,
  getCoreRowModel,
  getFilteredRowModel,
  getPaginationRowModel,
  getSortedRowModel,
  useReactTable,
} from "@tanstack/react-table"
import {
  ArrowDownIcon,
  ArrowUpIcon,
  ChevronLeftIcon,
  ChevronRightIcon,
  ChevronsUpDownIcon,
  ListFilterIcon,
  SearchIcon,
} from "lucide-react"

import { cn } from "@/lib/utils"
import { Button } from "@/components/ui/button"
import {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { Input } from "@/components/ui/input"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"

// meta.align is retained for column defs but no longer drives layout:
// alignment is derived from the column id (only "actions" is right-aligned).
declare module "@tanstack/react-table" {
  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  interface ColumnMeta<TData extends unknown, TValue> {
    align?: "left" | "right"
  }
}

export type DataTableFilter = {
  /** The column id to filter on (its value must match an option value). */
  columnId: string
  label: string
  options: { label: string; value: string }[]
}

const PAGE_SIZES = [10, 20, 50, 100]

export function DataTable<TData, TValue>({
  columns,
  data,
  searchPlaceholder = "Search…",
  searchKeys,
  searchable = true,
  filters = [],
  loading = false,
  emptyText = "Nothing here yet.",
  initialPageSize = 10,
  actions,
  fillHeight = false,
}: {
  columns: ColumnDef<TData, TValue>[]
  data: TData[]
  searchPlaceholder?: string
  /** Row fields the global search matches against (defaults to all string cells). */
  searchKeys?: (keyof TData)[]
  /** Hide the built-in search box (e.g. when the page already searches server-side). */
  searchable?: boolean
  filters?: DataTableFilter[]
  loading?: boolean
  emptyText?: string
  initialPageSize?: number
  actions?: React.ReactNode
  /** Fill the parent's height: the rows scroll inside the bordered box with a
   *  sticky header, while the toolbar and pagination stay pinned. Use inside a
   *  <Page variant="fill">. */
  fillHeight?: boolean
}) {
  const [sorting, setSorting] = React.useState<SortingState>([])
  const [columnFilters, setColumnFilters] = React.useState<ColumnFiltersState>([])
  const [globalFilter, setGlobalFilter] = React.useState("")

  const table = useReactTable({
    data,
    columns,
    state: { sorting, columnFilters, globalFilter },
    onSortingChange: setSorting,
    onColumnFiltersChange: setColumnFilters,
    onGlobalFilterChange: setGlobalFilter,
    globalFilterFn: (row, _columnId, value) => {
      const needle = String(value).toLowerCase().trim()
      if (!needle) return true
      const keys = searchKeys ?? (Object.keys(row.original as object) as (keyof TData)[])
      return keys.some((k) => {
        const v = (row.original as TData)[k]
        return typeof v === "string" && v.toLowerCase().includes(needle)
      })
    },
    getCoreRowModel: getCoreRowModel(),
    getSortedRowModel: getSortedRowModel(),
    getFilteredRowModel: getFilteredRowModel(),
    getPaginationRowModel: getPaginationRowModel(),
    initialState: { pagination: { pageSize: initialPageSize } },
  })

  const rows = table.getRowModel().rows
  const total = table.getFilteredRowModel().rows.length
  const { pageIndex, pageSize } = table.getState().pagination
  const first = total === 0 ? 0 : pageIndex * pageSize + 1
  const last = Math.min((pageIndex + 1) * pageSize, total)

  const showToolbar = searchable || filters.length > 0 || !!actions

  // Fill mode renders the header and body as SEPARATE tables so the scrollbar
  // sits beside the rows only (not over the header) and the header underline
  // stays put. We mirror the body's natural column widths onto the header and
  // reserve the body's scrollbar width behind the header background, so the
  // two stay pixel-aligned.
  const bodyScrollRef = React.useRef<HTMLDivElement>(null)
  const headerScrollRef = React.useRef<HTMLDivElement>(null)
  const bodyTableRef = React.useRef<HTMLTableElement>(null)
  const [colWidths, setColWidths] = React.useState<number[]>([])
  const [scrollbarW, setScrollbarW] = React.useState(0)

  React.useEffect(() => {
    if (!fillHeight) return
    const measure = () => {
      const scroller = bodyScrollRef.current
      if (scroller) {
        const sbw = scroller.offsetWidth - scroller.clientWidth
        setScrollbarW((p) => (Math.abs(p - sbw) < 0.5 ? p : sbw))
      }
      const firstRow = bodyTableRef.current?.querySelector<HTMLTableRowElement>(
        "tbody > tr[data-row]",
      )
      if (firstRow && firstRow.children.length === columns.length) {
        const widths = Array.from(firstRow.children).map(
          (c) => (c as HTMLElement).getBoundingClientRect().width,
        )
        setColWidths((p) =>
          p.length === widths.length && p.every((w, i) => Math.abs(w - widths[i]) < 0.5)
            ? p
            : widths,
        )
      }
    }
    measure()
    const ro = new ResizeObserver(measure)
    const s = bodyScrollRef.current
    const t = bodyTableRef.current
    if (s) ro.observe(s)
    if (t) ro.observe(t)
    return () => ro.disconnect()
  }, [fillHeight, rows.length, columns.length, pageIndex, pageSize])

  // Shared cell renderers so header/body markup is identical in both modes.
  const renderHeadCell = (header: Header<TData, unknown>) => {
    const canSort = header.column.getCanSort()
    const sorted = header.column.getIsSorted()
    // Only the trailing actions column is right-aligned; data stays left.
    const right = header.column.id === "actions"
    return (
      <TableHead
        key={header.id}
        className={cn("h-10 text-xs font-medium text-muted-foreground", right && "text-right")}
      >
        {header.isPlaceholder ? null : canSort ? (
          <button
            type="button"
            onClick={header.column.getToggleSortingHandler()}
            className={cn(
              "flex cursor-pointer items-center gap-1 select-none hover:text-primary",
              right && "ml-auto flex-row-reverse",
              sorted && "text-primary",
            )}
          >
            {flexRender(header.column.columnDef.header, header.getContext())}
            {sorted === "asc" ? (
              <ArrowUpIcon className="size-3.5" />
            ) : sorted === "desc" ? (
              <ArrowDownIcon className="size-3.5" />
            ) : (
              <ChevronsUpDownIcon className="size-3.5 opacity-50" />
            )}
          </button>
        ) : (
          flexRender(header.column.columnDef.header, header.getContext())
        )}
      </TableHead>
    )
  }

  const bodyRowsNode = loading ? (
    Array.from({ length: 5 }).map((_, i) => (
      <TableRow key={i} className="hover:bg-transparent">
        {columns.map((_c, j) => (
          <TableCell key={j} className="py-3">
            <div className="h-4 w-2/3 animate-pulse rounded bg-muted" />
          </TableCell>
        ))}
      </TableRow>
    ))
  ) : rows.length === 0 ? (
    <TableRow className="hover:bg-transparent">
      <TableCell
        colSpan={columns.length}
        className="py-10 text-center text-sm text-muted-foreground"
      >
        {emptyText}
      </TableCell>
    </TableRow>
  ) : (
    rows.map((row) => (
      <TableRow key={row.id} data-row className="group">
        {row.getVisibleCells().map((cell) => (
          <TableCell
            key={cell.id}
            className={cn("py-3", cell.column.id === "actions" && "text-right")}
          >
            {flexRender(cell.column.columnDef.cell, cell.getContext())}
          </TableCell>
        ))}
      </TableRow>
    ))
  )

  return (
    <div className={cn("flex flex-col gap-3", fillHeight && "min-h-0 flex-1")}>
      {/* Toolbar: search + filters (left), actions (right). */}
      {showToolbar && (
      <div className={cn("flex flex-wrap items-center gap-2", fillHeight && "shrink-0")}>
        {searchable && (
          <div className="relative w-full md:w-1/3">
            <SearchIcon className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground" />
            <Input
              value={globalFilter}
              onChange={(e) => setGlobalFilter(e.target.value)}
              placeholder={searchPlaceholder}
              className="h-8 pl-9"
            />
          </div>
        )}
        {filters.map((f) => {
          const col = table.getColumn(f.columnId)
          const active = (col?.getFilterValue() as string | undefined) ?? ""
          const activeLabel = f.options.find((o) => o.value === active)?.label
          return (
            <DropdownMenu key={f.columnId}>
              <DropdownMenuTrigger asChild>
                <Button
                  variant="outline"
                  size="sm"
                  className={cn("gap-2", active && "border-primary/40 text-primary")}
                >
                  <ListFilterIcon className="size-4" />
                  {activeLabel ?? f.label}
                  <ChevronsUpDownIcon className="size-3.5 opacity-60" />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="start" className="min-w-44">
                <DropdownMenuLabel className="text-xs text-muted-foreground">
                  {f.label}
                </DropdownMenuLabel>
                <DropdownMenuCheckboxItem
                  checked={!active}
                  onCheckedChange={() => col?.setFilterValue(undefined)}
                >
                  All
                </DropdownMenuCheckboxItem>
                <DropdownMenuSeparator />
                {f.options.map((o) => (
                  <DropdownMenuCheckboxItem
                    key={o.value}
                    checked={active === o.value}
                    onCheckedChange={() =>
                      col?.setFilterValue(active === o.value ? undefined : o.value)
                    }
                  >
                    {o.label}
                  </DropdownMenuCheckboxItem>
                ))}
              </DropdownMenuContent>
            </DropdownMenu>
          )
        })}
        {actions && <div className="ml-auto flex items-center gap-2">{actions}</div>}
      </div>
      )}

      {/* Table. */}
      {fillHeight ? (
        <div className="flex min-h-0 flex-1 flex-col overflow-hidden rounded-lg border">
          {/* Header: full width, its bottom border stays put. The body's
              scrollbar width is reserved behind the header background so the
              two tables line up; horizontal scroll is mirrored from the body. */}
          <div
            ref={headerScrollRef}
            className="shrink-0 overflow-x-auto overflow-y-hidden border-b bg-muted [&::-webkit-scrollbar]:hidden"
            style={{ paddingRight: scrollbarW, scrollbarWidth: "none" }}
          >
            <table
              className="w-full caption-bottom text-sm"
              style={colWidths.length ? { tableLayout: "fixed" } : undefined}
            >
              {colWidths.length > 0 && (
                <colgroup>
                  {colWidths.map((w, i) => (
                    <col key={i} style={{ width: `${w}px` }} />
                  ))}
                </colgroup>
              )}
              <TableHeader>
                {table.getHeaderGroups().map((hg) => (
                  <TableRow key={hg.id} className="border-0 hover:bg-transparent">
                    {hg.headers.map(renderHeadCell)}
                  </TableRow>
                ))}
              </TableHeader>
            </table>
          </div>
          {/* Body: the only scroller, so the scrollbar sits beside the rows. */}
          <div
            ref={bodyScrollRef}
            className="min-h-0 flex-1 overflow-auto"
            onScroll={(e) => {
              if (headerScrollRef.current)
                headerScrollRef.current.scrollLeft = e.currentTarget.scrollLeft
            }}
          >
            <table ref={bodyTableRef} className="w-full caption-bottom text-sm">
              <TableBody>{bodyRowsNode}</TableBody>
            </table>
          </div>
        </div>
      ) : (
        <div className="overflow-hidden rounded-lg border">
          <Table>
            <TableHeader className="bg-muted/60">
              {table.getHeaderGroups().map((hg) => (
                <TableRow key={hg.id} className="hover:bg-transparent">
                  {hg.headers.map(renderHeadCell)}
                </TableRow>
              ))}
            </TableHeader>
            <TableBody>{bodyRowsNode}</TableBody>
          </Table>
        </div>
      )}

      {/* Footer: range + rows-per-page (left), prev / page / next (right). */}
      <div
        className={cn(
          "flex flex-wrap items-center gap-3 text-sm text-muted-foreground",
          fillHeight && "shrink-0 border-t pt-3",
        )}
      >
        <span>
          Showing {first}–{last} of {total}
        </span>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button variant="outline" size="sm" className="gap-1.5">
              {pageSize} / page
              <ChevronsUpDownIcon className="size-3.5 opacity-60" />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start">
            {PAGE_SIZES.map((s) => (
              <DropdownMenuCheckboxItem
                key={s}
                checked={pageSize === s}
                onCheckedChange={() => table.setPageSize(s)}
              >
                {s} per page
              </DropdownMenuCheckboxItem>
            ))}
          </DropdownMenuContent>
        </DropdownMenu>
        <div className="ml-auto flex items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={() => table.previousPage()}
            disabled={!table.getCanPreviousPage()}
          >
            <ChevronLeftIcon className="size-4" /> Previous
          </Button>
          <span className="tabular-nums">
            Page {pageIndex + 1} of {table.getPageCount() || 1}
          </span>
          <Button
            variant="outline"
            size="sm"
            onClick={() => table.nextPage()}
            disabled={!table.getCanNextPage()}
          >
            Next <ChevronRightIcon className="size-4" />
          </Button>
        </div>
      </div>
    </div>
  )
}
