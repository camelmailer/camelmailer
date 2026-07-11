"use client"

// Instance admin: IP pools and their addresses.

import { useState } from "react"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { PlusIcon } from "lucide-react"
import { toast } from "sonner"
import { ConfirmDialog, EmptyState, PageHeader } from "@/components/shared"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
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
import { adminApi, ApiError, type IpPool } from "@/lib/api"

function errorToast(err: unknown, fallback: string) {
  toast.error(err instanceof ApiError ? err.message : fallback)
}

function PoolCard({ pool }: { pool: IpPool }) {
  const queryClient = useQueryClient()
  const addresses = useQuery({
    queryKey: ["admin", "ip-addresses", pool.id],
    queryFn: () => adminApi.ipPools.addresses(pool.id).list(),
  })
  const [open, setOpen] = useState(false)
  const [ipv4, setIpv4] = useState("")
  const [ipv6, setIpv6] = useState("")
  const [hostname, setHostname] = useState("")
  const [priority, setPriority] = useState("100")
  const [deletePool, setDeletePool] = useState(false)

  const invalidate = () => {
    queryClient.invalidateQueries({ queryKey: ["admin", "ip-addresses", pool.id] })
    queryClient.invalidateQueries({ queryKey: ["admin", "ip-pools"] })
  }

  const addAddress = useMutation({
    mutationFn: () =>
      adminApi.ipPools.addresses(pool.id).create({
        ipv4,
        ...(ipv6 ? { ipv6 } : {}),
        hostname,
        priority: Number(priority) || 100,
      }),
    onSuccess: () => {
      invalidate()
      setOpen(false)
      setIpv4("")
      setIpv6("")
      setHostname("")
    },
    onError: (err) => errorToast(err, "Could not add the address"),
  })

  return (
    <Card>
      <CardHeader className="flex-row items-center justify-between">
        <CardTitle className="flex items-center gap-2 text-base">
          {pool.name}
          {pool.default && <Badge>default</Badge>}
        </CardTitle>
        <div className="flex gap-2">
          <Button variant="outline" size="sm" onClick={() => setOpen(true)}>
            <PlusIcon className="size-4" /> Address
          </Button>
          <Button variant="ghost" size="sm" onClick={() => setDeletePool(true)}>
            Delete pool
          </Button>
        </div>
      </CardHeader>
      <CardContent>
        {addresses.data?.ip_addresses.length === 0 ? (
          <p className="text-sm text-muted-foreground">No addresses in this pool.</p>
        ) : (
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>IPv4</TableHead>
                <TableHead>IPv6</TableHead>
                <TableHead>Hostname (EHLO)</TableHead>
                <TableHead>Priority</TableHead>
                <TableHead />
              </TableRow>
            </TableHeader>
            <TableBody>
              {addresses.data?.ip_addresses.map((address) => (
                <TableRow key={address.id}>
                  <TableCell className="font-mono text-xs">{address.ipv4}</TableCell>
                  <TableCell className="font-mono text-xs">{address.ipv6 ?? "—"}</TableCell>
                  <TableCell>{address.hostname}</TableCell>
                  <TableCell>{address.priority}</TableCell>
                  <TableCell className="text-right">
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={async () => {
                        try {
                          await adminApi.ipPools.addresses(pool.id).delete(address.id)
                          invalidate()
                        } catch (err) {
                          errorToast(err, "Could not delete the address")
                        }
                      }}
                    >
                      Delete
                    </Button>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        )}
      </CardContent>

      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Add IP address to {pool.name}</DialogTitle>
          </DialogHeader>
          <div className="grid gap-4">
            <div className="grid grid-cols-2 gap-2">
              <div className="grid gap-2">
                <Label>IPv4</Label>
                <Input value={ipv4} onChange={(e) => setIpv4(e.target.value)} placeholder="203.0.113.10" />
              </div>
              <div className="grid gap-2">
                <Label>IPv6 (optional)</Label>
                <Input value={ipv6} onChange={(e) => setIpv6(e.target.value)} />
              </div>
            </div>
            <div className="grid grid-cols-2 gap-2">
              <div className="grid gap-2">
                <Label>Hostname</Label>
                <Input
                  value={hostname}
                  onChange={(e) => setHostname(e.target.value)}
                  placeholder="mx1.example.com"
                />
              </div>
              <div className="grid gap-2">
                <Label>Priority</Label>
                <Input value={priority} onChange={(e) => setPriority(e.target.value)} />
              </div>
            </div>
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setOpen(false)}>
              Cancel
            </Button>
            <Button
              onClick={() => addAddress.mutate()}
              disabled={addAddress.isPending || !ipv4 || !hostname}
            >
              Add
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <ConfirmDialog
        open={deletePool}
        onOpenChange={setDeletePool}
        title={`Delete pool ${pool.name}?`}
        description="Servers assigned to this pool fall back to the default pool."
        onConfirm={async () => {
          try {
            await adminApi.ipPools.delete(pool.id)
            invalidate()
          } catch (err) {
            errorToast(err, "Could not delete the pool")
          }
        }}
      />
    </Card>
  )
}

export default function IpPools() {
  const queryClient = useQueryClient()
  const pools = useQuery({ queryKey: ["admin", "ip-pools"], queryFn: adminApi.ipPools.list })
  const [open, setOpen] = useState(false)
  const [name, setName] = useState("")
  const [isDefault, setIsDefault] = useState(false)

  const create = useMutation({
    mutationFn: () => adminApi.ipPools.create(name, isDefault),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["admin", "ip-pools"] })
      setOpen(false)
      setName("")
      setIsDefault(false)
    },
    onError: (err) => errorToast(err, "Could not create the pool"),
  })

  return (
    <div className="space-y-4">
      <PageHeader
        title="IP pools"
        description="Source addresses for outbound mail; assign pools per server."
        action={
          <Button size="sm" onClick={() => setOpen(true)}>
            <PlusIcon className="size-4" /> New pool
          </Button>
        }
      />
      {pools.data?.ip_pools.length === 0 ? (
        <EmptyState>No IP pools. Without pools, outbound mail uses the host address.</EmptyState>
      ) : (
        pools.data?.ip_pools.map((pool) => <PoolCard key={pool.id} pool={pool} />)
      )}

      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>New IP pool</DialogTitle>
          </DialogHeader>
          <div className="grid gap-4">
            <div className="grid gap-2">
              <Label>Name</Label>
              <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="Transactional" />
            </div>
            <div className="flex items-center gap-2">
              <Switch checked={isDefault} onCheckedChange={setIsDefault} id="default-pool" />
              <Label htmlFor="default-pool">Default pool</Label>
            </div>
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setOpen(false)}>
              Cancel
            </Button>
            <Button onClick={() => create.mutate()} disabled={create.isPending || !name.trim()}>
              Create
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}
