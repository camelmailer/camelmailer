"use client"

// A reusable, scanning-optimized data table (shadcn + TanStack Table),
// following the datatable-ux rules: prominent search, optional Filter
// dropdowns for a primary dimension, sortable headers with a visible
// affordance, a distinct tinted header, roomy hairline-divided rows, a
// one-scale text-sm chrome, right-aligned tabular numbers (via column
// meta.align), real loading/empty states and a client-side pagination
// footer (rows-per-page + prev/next). Every dashboard table reuses this.

import * as React from "react"
import {
  type ColumnDef,
  type ColumnFiltersState,
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

// Columns opt into right alignment (numbers) via meta.align = "right".
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
  filters = [],
  loading = false,
  emptyText = "Nothing here yet.",
  initialPageSize = 10,
  actions,
}: {
  columns: ColumnDef<TData, TValue>[]
  data: TData[]
  searchPlaceholder?: string
  /** Row fields the global search matches against (defaults to all string cells). */
  searchKeys?: (keyof TData)[]
  filters?: DataTableFilter[]
  loading?: boolean
  emptyText?: string
  initialPageSize?: number
  actions?: React.ReactNode
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

  return (
    <div className="flex flex-col gap-3">
      {/* Toolbar: search + filters (left), actions (right). */}
      <div className="flex flex-wrap items-center gap-2">
        <div className="relative w-full max-w-xs">
          <SearchIcon className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input
            value={globalFilter}
            onChange={(e) => setGlobalFilter(e.target.value)}
            placeholder={searchPlaceholder}
            className="pl-9"
          />
        </div>
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

      {/* Table. */}
      <div className="overflow-hidden rounded-lg border">
        <Table>
          <TableHeader className="bg-muted/60">
            {table.getHeaderGroups().map((hg) => (
              <TableRow key={hg.id} className="hover:bg-transparent">
                {hg.headers.map((header) => {
                  const canSort = header.column.getCanSort()
                  const sorted = header.column.getIsSorted()
                  const right = header.column.columnDef.meta?.align === "right"
                  return (
                    <TableHead
                      key={header.id}
                      className={cn(
                        "h-10 text-xs font-medium tracking-wide text-muted-foreground uppercase",
                        right && "text-right",
                      )}
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
                })}
              </TableRow>
            ))}
          </TableHeader>
          <TableBody>
            {loading ? (
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
                <TableRow key={row.id} className="group">
                  {row.getVisibleCells().map((cell) => (
                    <TableCell
                      key={cell.id}
                      className={cn(
                        "py-3",
                        cell.column.columnDef.meta?.align === "right" &&
                          "text-right tabular-nums",
                      )}
                    >
                      {flexRender(cell.column.columnDef.cell, cell.getContext())}
                    </TableCell>
                  ))}
                </TableRow>
              ))
            )}
          </TableBody>
        </Table>
      </div>

      {/* Footer: range + rows-per-page (left), prev / page / next (right). */}
      <div className="flex flex-wrap items-center gap-3 text-sm text-muted-foreground">
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
