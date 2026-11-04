import { describe, expect, it } from "vitest"
import {
  constructTable,
  tableFeatures,
  type ColumnDef,
  type TableOptions,
} from "@tanstack/react-table"
import { storeReactivityBindings } from "@tanstack/table-core/store-reactivity-bindings"
import { dataTableFeatures, searchRows } from "./data-table-features"

// The dashboard's DataTable on TanStack Table v9, without React: the same
// features, row models and function registries, driven through the table
// API the component calls. Guards the behaviour the v8 table had.

type Row = { id: string; name: string; count: number; status: string }

// React supplies the reactivity in the app; here the store bindings for
// framework-free use do.
const features = tableFeatures({
  ...dataTableFeatures,
  coreReactivityFeature: storeReactivityBindings(),
})
type Features = typeof features

const rows: Row[] = [
  { id: "a", name: "server 10", count: 3, status: "active" },
  { id: "b", name: "server 2", count: 12, status: "paused" },
  { id: "c", name: "Alpha", count: 7, status: "active" },
  { id: "d", name: "beta", count: 1, status: "Active" },
]

const columns: ColumnDef<Features, Row>[] = [
  { accessorKey: "name", header: "Name" },
  { accessorKey: "count", header: "Count" },
  { accessorKey: "status", header: "Status", filterFn: "equalsString" },
]

function table(overrides: Partial<TableOptions<Features, Row>> = {}) {
  const t = constructTable<Features, Row>({
    features,
    data: rows,
    columns,
    getRowId: (row) => row.id,
    globalFilterFn: searchRows<Row, Features>(["name"]),
    enableRowSelection: true,
    initialState: { pagination: { pageIndex: 0, pageSize: 2 } },
    ...overrides,
  })
  return t
}

const names = (t: ReturnType<typeof table>) => t.getRowModel().rows.map((r) => r.original.name)

describe("DataTable features (TanStack Table v9)", () => {
  it("sorts text columns alphanumerically and number columns numerically", () => {
    const t = table({ initialState: { pagination: { pageIndex: 0, pageSize: 10 } } })
    t.setSorting([{ id: "name", desc: false }])
    expect(names(t)).toEqual(["Alpha", "beta", "server 2", "server 10"])
    t.setSorting([{ id: "count", desc: true }])
    expect(t.getRowModel().rows.map((r) => r.original.count)).toEqual([12, 7, 3, 1])
  })

  it("filters a column by its auto function and by a named one", () => {
    const t = table({ initialState: { pagination: { pageIndex: 0, pageSize: 10 } } })
    t.getColumn("name")!.setFilterValue("SERV")
    expect(names(t)).toEqual(["server 10", "server 2"])
    t.setColumnFilters([])
    // equalsString: an exact option value, case-insensitive
    t.getColumn("status")!.setFilterValue("active")
    expect(names(t)).toEqual(["server 10", "Alpha", "beta"])
  })

  it("searches the configured fields only", () => {
    const t = table({ initialState: { pagination: { pageIndex: 0, pageSize: 10 } } })
    t.setGlobalFilter("alp")
    expect(names(t)).toEqual(["Alpha"])
    t.setGlobalFilter("paused")
    expect(names(t)).toEqual([])
  })

  it("pages through the filtered rows", () => {
    const t = table()
    expect(t.getPageCount()).toBe(2)
    expect(names(t)).toEqual(["server 10", "server 2"])
    expect(t.getCanPreviousPage()).toBe(false)
    t.nextPage()
    expect(names(t)).toEqual(["Alpha", "beta"])
    expect(t.getCanNextPage()).toBe(false)
    t.setPageSize(10)
    expect(t.getRowModel().rows).toHaveLength(4)
  })

  it("selects the rows of the current page, and a full page is not mixed", () => {
    const t = table()
    t.getRowModel().rows[0].toggleSelected(true)
    expect(t.getIsSomePageRowsSelected()).toBe(true)
    expect(t.getIsAllPageRowsSelected()).toBe(false)
    t.toggleAllPageRowsSelected(true)
    expect(t.getIsAllPageRowsSelected()).toBe(true)
    expect(t.getSelectedRowModel().rows.map((r) => r.id)).toEqual(["a", "b"])
  })
})
