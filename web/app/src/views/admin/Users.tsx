"use client"

// Instance admin: global user accounts.

import { useState } from "react"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { PlusIcon } from "lucide-react"
import { toast } from "sonner"
import { ConfirmDialog, PageHeader } from "@/components/shared"
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
import { Switch } from "@/components/ui/switch"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import { adminApi, ApiError } from "@/lib/api"

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
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Name</TableHead>
            <TableHead>Email</TableHead>
            <TableHead>Admin</TableHead>
            <TableHead />
          </TableRow>
        </TableHeader>
        <TableBody>
          {users.data?.users.map((user) => (
            <TableRow key={user.id}>
              <TableCell>
                {user.first_name} {user.last_name}
              </TableCell>
              <TableCell className="text-muted-foreground">{user.email_address}</TableCell>
              <TableCell>
                <Switch
                  checked={user.admin}
                  onCheckedChange={async (checked) => {
                    try {
                      await adminApi.users.update(user.id, { admin: checked })
                      invalidate()
                    } catch (err) {
                      toast.error(
                        err instanceof ApiError ? err.message : "Could not update the user",
                      )
                    }
                  }}
                />
              </TableCell>
              <TableCell className="text-right">
                <Button variant="ghost" size="sm" onClick={() => setDeleteId(user.id)}>
                  Delete
                </Button>
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>

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
                placeholder="min. 8 characters — empty for SSO-only accounts"
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
