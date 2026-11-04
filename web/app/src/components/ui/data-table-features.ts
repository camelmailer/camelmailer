import {
  type FilterFn,
  type RowData,
  type TableFeatures,
  columnFilteringFeature,
  createFilteredRowModel,
  createPaginatedRowModel,
  createSortedRowModel,
  filterFn_arrIncludes,
  filterFn_equals,
  filterFn_equalsString,
  filterFn_inDateRange,
  filterFn_inNumberRange,
  filterFn_includesString,
  filterFn_weakEquals,
  globalFilteringFeature,
  rowPaginationFeature,
  rowSelectionFeature,
  rowSortingFeature,
  sortFn_alphanumeric,
  sortFn_basic,
  sortFn_datetime,
  sortFn_text,
  tableFeatures,
} from "@tanstack/react-table"

/**
 * What every dashboard table can do. TanStack Table v9 registers features
 * explicitly: sorting, a filter per column, the global search, pagination
 * and row selection. The function registries hold the built-ins that
 * `"auto"` resolves to (and the one a column names), so sorting and column
 * filters behave as they did on v8.
 */
export const dataTableFeatures = tableFeatures({
  rowSortingFeature,
  columnFilteringFeature,
  globalFilteringFeature,
  rowPaginationFeature,
  rowSelectionFeature,
  sortedRowModel: createSortedRowModel(),
  filteredRowModel: createFilteredRowModel(),
  paginatedRowModel: createPaginatedRowModel(),
  sortFns: {
    alphanumeric: sortFn_alphanumeric,
    text: sortFn_text,
    datetime: sortFn_datetime,
    basic: sortFn_basic,
  },
  filterFns: {
    includesString: filterFn_includesString,
    inNumberRange: filterFn_inNumberRange,
    equals: filterFn_equals,
    arrIncludes: filterFn_arrIncludes,
    inDateRange: filterFn_inDateRange,
    weakEquals: filterFn_weakEquals,
    // named by columns that filter on an exact option value
    equalsString: filterFn_equalsString,
  },
})

export type DataTableFeatures = typeof dataTableFeatures

/**
 * The table's search box: a row matches when one of `keys` (default: every
 * field of the row) is a string containing the search text, case-insensitive.
 */
export function searchRows<
  TData extends RowData,
  TFeatures extends TableFeatures = DataTableFeatures,
>(keys?: (keyof TData)[]): FilterFn<TFeatures, TData> {
  return (row, _columnId, value) => {
    const needle = String(value).toLowerCase().trim()
    if (!needle) return true
    const fields = keys ?? (Object.keys(row.original) as (keyof TData)[])
    return fields.some((k) => {
      const v = row.original[k]
      return typeof v === "string" && v.toLowerCase().includes(needle)
    })
  }
}
