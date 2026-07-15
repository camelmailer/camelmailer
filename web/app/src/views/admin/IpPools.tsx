"use client"

// Instance admin: IP pools and their addresses.

import { useState } from "react"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { type ColumnDef } from "@tanstack/react-table"
import { PlusIcon, Trash2Icon } from "lucide-react"
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
import { adminApi, ApiError, type IpAddress, type IpPool } from "@/lib/api"

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

  const columns: ColumnDef<IpAddress>[] = [
    {
      id: "ipv4",
      header: "IPv4",
      accessorFn: (r) => r.ipv4,
      cell: ({ row }) => (
        <span className="block truncate font-mono text-xs font-medium">{row.original.ipv4}</span>
      ),
    },
    {
      id: "ipv6",
      header: "IPv6",
      accessorFn: (r) => r.ipv6 ?? "",
      cell: ({ row }) => (
        <span className="font-mono text-xs text-muted-foreground">{row.original.ipv6 ?? "—"}</span>
      ),
    },
    {
      id: "hostname",
      header: "Hostname (EHLO)",
      accessorFn: (r) => r.hostname,
      cell: ({ row }) => <span>{row.original.hostname}</span>,
    },
    {
      id: "priority",
      header: "Priority",
      accessorFn: (r) => r.priority,
      meta: { align: "right" },
      cell: ({ row }) => <span>{row.original.priority}</span>,
    },
    {
      id: "actions",
      header: "",
      enableSorting: false,
      meta: { align: "right" },
      cell: ({ row }) => (
        <div onClick={(e) => e.stopPropagation()}>
          <Button
            variant="ghost"
            size="icon"
            onClick={async () => {
              try {
                await adminApi.ipPools.addresses(pool.id).delete(row.original.id)
                invalidate()
              } catch (err) {
                errorToast(err, "Could not delete the address")
              }
            }}
          >
            <Trash2Icon className="size-4" />
            <span className="sr-only">Delete</span>
          </Button>
        </div>
      ),
    },
  ]

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
        <DataTable
          columns={columns}
          data={addresses.data?.ip_addresses ?? []}
          loading={addresses.isPending}
          searchKeys={["ipv4", "ipv6", "hostname"]}
          searchPlaceholder="Search addresses…"
          emptyText="No addresses in this pool."
          initialPageSize={20}
        />
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
