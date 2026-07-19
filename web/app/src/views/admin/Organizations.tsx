"use client"

// Instance admin: every organization on this instance.

import { useQuery } from "@tanstack/react-query"
import { type ColumnDef } from "@tanstack/react-table"
import { PageHeader } from "@/components/shared"
import { Page } from "@/components/page"
import { Badge } from "@/components/ui/badge"
import { DataTable } from "@/components/ui/data-table"
import { adminApi, type Organization } from "@/lib/api"

export default function Organizations() {
  const organizations = useQuery({
    queryKey: ["admin", "organizations"],
    queryFn: adminApi.organizations.list,
  })

  const rows: Organization[] = organizations.data?.organizations ?? []

  const columns: ColumnDef<Organization>[] = [
    {
      id: "name",
      header: "Name",
      accessorFn: (r) => r.name,
      cell: ({ row }) => (
        <span className="block truncate font-medium">{row.original.name}</span>
      ),
    },
    {
      id: "permalink",
      header: "Permalink",
      accessorFn: (r) => r.permalink,
      cell: ({ row }) => (
        <span className="text-muted-foreground font-mono text-xs">
          {row.original.permalink}
        </span>
      ),
    },
    {
      id: "two_factor",
      header: "Two-factor",
      accessorFn: (r) => r.require_two_factor,
      enableSorting: false,
      filterFn: (row, _id, value) =>
        value === "enforced" ? row.original.require_two_factor : !row.original.require_two_factor,
      cell: ({ row }) =>
        row.original.require_two_factor ? (
          <Badge variant="secondary">Enforced</Badge>
        ) : (
          <span className="text-muted-foreground">—</span>
        ),
    },
  ]

  return (
    <Page
      variant="fill"
      header={
        <PageHeader
          title="Organizations"
          description="Every organization on this instance."
          className="mb-0"
        />
      }
    >
      <div className="flex min-h-0 flex-1 flex-col">
        <DataTable
          columns={columns}
          data={rows}
          loading={organizations.isPending}
          searchKeys={["name", "permalink"]}
          searchPlaceholder="Search organizations…"
          emptyText="No organizations on this instance yet."
          filters={[
            {
              columnId: "two_factor",
              label: "2FA",
              options: [
                { label: "Enforced", value: "enforced" },
                { label: "Not enforced", value: "off" },
              ],
            },
          ]}
          fillHeight
          initialPageSize={20}
        />
      </div>
    </Page>
  )
}
