"use client"

// Instance admin: global user accounts.

import { useState } from "react"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { type ColumnDef } from "@tanstack/react-table"
import { PlusIcon, Trash2Icon } from "lucide-react"
import { toast } from "sonner"
import { ConfirmDialog, PageHeader } from "@/components/shared"
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
import { Switch } from "@/components/ui/switch"
import { adminApi, ApiError, type User } from "@/lib/api"

type UserRow = User & { name: string }

export default function Users() {
  const queryClient = useQueryClient()
  const users = useQuery({ queryKey: ["admin", "users"], queryFn: adminApi.users.list })
  const [open, setOpen] = useState(false)
  const [email, setEmail] = useState("")
  const [firstName, setFirstName] = useState("")
  const [lastName, setLastName] = useState("")
  const [password, setPassword] = useState("")
  const [isAdmin, setIsAdmin] = useState(false)
  const [deleteId, setDeleteId] = useState<number | null>(null)

  const invalidate = () => queryClient.invalidateQueries({ queryKey: ["admin", "users"] })

  const create = useMutation({
    mutationFn: () =>
      adminApi.users.create({
        email_address: email,
        first_name: firstName,
        last_name: lastName,
        admin: isAdmin,
        ...(password ? { password } : {}),
      }),
    onSuccess: () => {
      invalidate()
      setOpen(false)
      setEmail("")
      setFirstName("")
      setLastName("")
      setPassword("")
      setIsAdmin(false)
    },
    onError: (err) =>
      toast.error(err instanceof ApiError ? err.message : "Could not create the user"),
  })

  const rows: UserRow[] = (users.data?.users ?? []).map((user) => ({
    ...user,
    name: `${user.first_name} ${user.last_name}`.trim(),
  }))

  const columns: ColumnDef<UserRow>[] = [
    {
      id: "name",
      header: "Name",
      accessorFn: (r) => r.name,
      cell: ({ row }) => (
        <span className="block truncate font-medium">{row.original.name || "—"}</span>
      ),
    },
    {
      id: "email",
      header: "Email",
      accessorFn: (r) => r.email_address,
      cell: ({ row }) => (
        <span className="text-muted-foreground">{row.original.email_address}</span>
      ),
    },
    {
      id: "admin",
      header: "Admin",
      accessorFn: (r) => r.admin,
      enableSorting: false,
      filterFn: (row, _id, value) =>
        value === "admin" ? row.original.admin : !row.original.admin,
      cell: ({ row }) => (
        <div onClick={(e) => e.stopPropagation()}>
          <Switch
            checked={row.original.admin}
            onCheckedChange={async (checked) => {
              try {
                await adminApi.users.update(row.original.id, { admin: checked })
                invalidate()
              } catch (err) {
                toast.error(
                  err instanceof ApiError ? err.message : "Could not update the user",
                )
              }
            }}
          />
        </div>
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
            <span className="sr-only">Delete</span>
          </Button>
        </div>
      ),
    },
  ]

  return (
    <div>
      <PageHeader
        title="Users"
        description="Every account on this instance."
        action={
          <Button size="sm" onClick={() => setOpen(true)}>
            <PlusIcon className="size-4" /> New user
          </Button>
        }
      />
      <DataTable
        columns={columns}
        data={rows}
        loading={users.isPending}
        searchKeys={["name", "email_address"]}
        searchPlaceholder="Search users…"
        emptyText="No users on this instance yet."
        filters={[
          {
            columnId: "admin",
            label: "Role",
            options: [
              { label: "Admin", value: "admin" },
              { label: "Member", value: "member" },
            ],
          },
        ]}
        initialPageSize={20}
      />

      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>New user</DialogTitle>
          </DialogHeader>
          <div className="grid gap-4">
            <div className="grid gap-2">
              <Label>Email address</Label>
              <Input value={email} onChange={(e) => setEmail(e.target.value)} />
            </div>
            <div className="grid grid-cols-2 gap-2">
              <div className="grid gap-2">
                <Label>First name</Label>
                <Input value={firstName} onChange={(e) => setFirstName(e.target.value)} />
              </div>
              <div className="grid gap-2">
                <Label>Last name</Label>
                <Input value={lastName} onChange={(e) => setLastName(e.target.value)} />
              </div>
            </div>
            <div className="grid gap-2">
              <Label>Initial password (optional)</Label>
              <Input
                type="password"
                autoComplete="new-password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                placeholder="min. 8 characters, or leave empty for SSO-only accounts"
              />
            </div>
            <div className="flex items-center gap-2">
              <Switch checked={isAdmin} onCheckedChange={setIsAdmin} id="is-admin" />
              <Label htmlFor="is-admin">Instance admin (full access)</Label>
            </div>
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setOpen(false)}>
              Cancel
            </Button>
            <Button
              onClick={() => create.mutate()}
              disabled={create.isPending || !email.includes("@")}
            >
              Create
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <ConfirmDialog
        open={deleteId !== null}
        onOpenChange={(open) => !open && setDeleteId(null)}
        title="Delete user"
        description="Removes the account, its sessions and memberships."
        onConfirm={async () => {
          try {
            await adminApi.users.delete(deleteId!)
            invalidate()
          } catch (err) {
            toast.error(err instanceof ApiError ? err.message : "Could not delete the user")
          }
        }}
      />
    </div>
  )
}
