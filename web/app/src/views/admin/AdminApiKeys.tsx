"use client"

// Instance admin: machine keys for the admin API.

import { useState } from "react"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { type ColumnDef } from "@tanstack/react-table"
import { PlusIcon, Trash2Icon } from "lucide-react"
import { toast } from "sonner"
import { ConfirmDialog, PageHeader, SecretReveal } from "@/components/shared"
import { Button } from "@/components/ui/button"
import { DataTable } from "@/components/ui/data-table"
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { adminApi, ApiError, type AdminApiKey } from "@/lib/api"

export default function AdminApiKeys() {
  const queryClient = useQueryClient()
  const keys = useQuery({ queryKey: ["admin", "api-keys"], queryFn: adminApi.adminApiKeys.list })
  const [open, setOpen] = useState(false)
  const [name, setName] = useState("")
  const [issued, setIssued] = useState<string | null>(null)
  const [deleteId, setDeleteId] = useState<number | null>(null)

  const invalidate = () => queryClient.invalidateQueries({ queryKey: ["admin", "api-keys"] })

  const create = useMutation({
    mutationFn: () => adminApi.adminApiKeys.create(name),
    onSuccess: ({ admin_api_key }) => {
      invalidate()
      setName("")
      setIssued(admin_api_key.key ?? null)
    },
    onError: (err) =>
      toast.error(err instanceof ApiError ? err.message : "Could not create the key"),
  })

  const columns: ColumnDef<AdminApiKey>[] = [
    {
      id: "name",
      header: "Name",
      accessorFn: (r) => r.name,
      cell: ({ row }) => <span className="block truncate font-medium">{row.original.name}</span>,
    },
    {
      id: "key",
      header: "Key",
      accessorFn: (r) => r.key_prefix,
      cell: ({ row }) => (
        <span className="font-mono text-xs text-muted-foreground">{row.original.key_prefix}…</span>
      ),
    },
    {
      id: "actions",
      header: "",
      enableSorting: false,
      meta: { align: "right" },
      cell: ({ row }) => (
        <div onClick={(e) => e.stopPropagation()}>
          <Button variant="ghost" size="icon" onClick={() => setDeleteId(row.original.id)}>
            <Trash2Icon className="size-4" />
            <span className="sr-only">Revoke</span>
          </Button>
        </div>
      ),
    },
  ]

  return (
    <div>
      <PageHeader
        title="Admin API keys"
        description="Machine credentials with full access to the admin API (X-Admin-API-Key)."
        action={
          <Button size="sm" onClick={() => { setIssued(null); setOpen(true) }}>
            <PlusIcon className="size-4" /> New key
          </Button>
        }
      />
      <DataTable
        columns={columns}
        data={keys.data?.admin_api_keys ?? []}
        loading={keys.isPending}
        searchKeys={["name", "key_prefix"]}
        searchPlaceholder="Search keys…"
        emptyText="No admin API keys yet."
        initialPageSize={20}
      />

      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>New admin API key</DialogTitle>
          </DialogHeader>
          {issued ? (
            <SecretReveal label="API key" value={issued} />
          ) : (
            <div className="grid gap-2">
              <Label>Name</Label>
              <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="ci" />
            </div>
          )}
          <DialogFooter>
            <Button variant="outline" onClick={() => setOpen(false)}>
              {issued ? "Done" : "Cancel"}
            </Button>
            {!issued && (
              <Button onClick={() => create.mutate()} disabled={create.isPending || !name.trim()}>
                Create
              </Button>
            )}
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <ConfirmDialog
        open={deleteId !== null}
        onOpenChange={(open) => !open && setDeleteId(null)}
        title="Revoke API key"
        description="Integrations using this key stop working immediately."
        confirmLabel="Revoke"
        onConfirm={async () => {
          try {
            await adminApi.adminApiKeys.delete(deleteId!)
            invalidate()
          } catch (err) {
            toast.error(err instanceof ApiError ? err.message : "Could not revoke the key")
          }
        }}
      />
    </div>
  )
}
