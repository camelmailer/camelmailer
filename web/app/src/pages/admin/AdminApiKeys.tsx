// Instance admin: machine keys for the admin API.

import { useState } from "react"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { PlusIcon } from "lucide-react"
import { toast } from "sonner"
import { ConfirmDialog, PageHeader, SecretReveal } from "@/components/shared"
import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import { adminApi, ApiError } from "@/lib/api"

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
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Name</TableHead>
            <TableHead>Key</TableHead>
            <TableHead />
          </TableRow>
        </TableHeader>
        <TableBody>
          {keys.data?.admin_api_keys.map((key) => (
            <TableRow key={key.id}>
              <TableCell>{key.name}</TableCell>
              <TableCell className="font-mono text-xs text-muted-foreground">
                {key.key_prefix}…
              </TableCell>
              <TableCell className="text-right">
                <Button variant="ghost" size="sm" onClick={() => setDeleteId(key.id)}>
                  Revoke
                </Button>
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>

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
