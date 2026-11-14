"use client"

// Instance admin: IP pools and their addresses.

import { useState } from "react"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { PlusIcon, Trash2Icon } from "lucide-react"
import { toast } from "sonner"
import { ConfirmDialog, EmptyState, PageHeader } from "@/components/shared"
import { Page } from "@/components/page"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import { DataTable, type DataTableColumn } from "@/components/ui/data-table"
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
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { Switch } from "@/components/ui/switch"
import {
  adminApi,
  ApiError,
  type AddressFamily,
  type IpAddress,
  type IpPool,
  type SourceAddressCheck,
} from "@/lib/api"
import { relativeTime } from "@/lib/api-p1"

function errorToast(err: unknown, fallback: string) {
  toast.error(err instanceof ApiError ? err.message : fallback)
}

// "default" stands for a pool without a family of its own (null).
const FAMILIES: { value: AddressFamily | "default"; label: string }[] = [
  { value: "default", label: "IPv4 address only (default)" },
  { value: "dual_stack", label: "Dual stack, IPv6 first" },
  { value: "prefer_ipv4", label: "Both, IPv4 first" },
  { value: "ipv4_only", label: "IPv4 only" },
  { value: "ipv6_only", label: "IPv6 only" },
]

const STATUS: Record<
  SourceAddressCheck["status"],
  { label: string; variant: "default" | "secondary" | "destructive" | "outline" }
> = {
  pass: { label: "PTR ok", variant: "secondary" },
  no_ptr: { label: "No PTR", variant: "destructive" },
  mismatch: { label: "PTR mismatch", variant: "destructive" },
  lookup_failed: { label: "Lookup failed", variant: "outline" },
  not_public: { label: "Not public", variant: "outline" },
}

function ReverseDnsCard() {
  const checks = useQuery({
    queryKey: ["admin", "source-addresses"],
    queryFn: adminApi.sourceAddresses.list,
  })
  const columns: DataTableColumn<SourceAddressCheck>[] = [
    {
      id: "address",
      header: "Address",
      accessorFn: (r) => r.address,
      cell: ({ row }) => (
        <span className="block truncate font-mono text-xs font-medium">{row.original.address}</span>
      ),
    },
    {
      id: "origin",
      header: "Used by",
      accessorFn: (r) => r.origin,
      cell: ({ row }) => (
        <span>{row.original.origin === "pool" ? "IP pool" : "Servers without a pool"}</span>
      ),
    },
    {
      id: "status",
      header: "Reverse DNS",
      accessorFn: (r) => r.status,
      cell: ({ row }) => {
        const status = STATUS[row.original.status] ?? { label: row.original.status, variant: "outline" }
        return (
          <Badge variant={status.variant} title={row.original.detail}>
            {status.label}
          </Badge>
        )
      },
    },
    {
      id: "ptr",
      header: "PTR",
      accessorFn: (r) => r.ptr.join(", "),
      cell: ({ row }) => (
        <span className="font-mono text-xs text-muted-foreground">
          {row.original.ptr.length ? row.original.ptr.join(", ") : "—"}
        </span>
      ),
    },
    {
      id: "checked_at",
      header: "Checked",
      accessorFn: (r) => r.checked_at,
      cell: ({ row }) => (
        <span className="text-muted-foreground" title={row.original.checked_by}>
          {relativeTime(row.original.checked_at)}
        </span>
      ),
    },
  ]
  const failing = checks.data?.failing ?? 0
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2 text-base">
          Reverse DNS of the source addresses
          {failing > 0 && <Badge variant="destructive">{failing} failing</Badge>}
        </CardTitle>
        <p className="text-sm text-muted-foreground">
          Each worker checks at start, and then hourly by default, that the addresses it sends from
          have a PTR record that resolves back to them. Receivers such as Microsoft 365 refuse mail
          from an address without one.
        </p>
      </CardHeader>
      <CardContent>
        <DataTable
          columns={columns}
          data={checks.data?.source_addresses ?? []}
          loading={checks.isPending}
          searchKeys={["address", "ptr"]}
          searchPlaceholder="Search addresses…"
          emptyText="No results yet. They appear once a worker has started."
          initialPageSize={20}
        />
      </CardContent>
    </Card>
  )
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

  const setFamily = useMutation({
    mutationFn: (value: AddressFamily | "default") =>
      adminApi.ipPools.update(pool.id, {
        address_family: value === "default" ? null : value,
      }),
    onSuccess: () => invalidate(),
    onError: (err) => errorToast(err, "Could not change the address family"),
  })

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

  const columns: DataTableColumn<IpAddress>[] = [
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
          <Select
            value={pool.address_family ?? "default"}
            onValueChange={(value) => setFamily.mutate(value as AddressFamily | "default")}
            disabled={setFamily.isPending}
          >
            <SelectTrigger size="sm" aria-label="Address family" className="w-56">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {FAMILIES.map((family) => (
                <SelectItem key={family.value} value={family.value}>
                  {family.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
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
    <Page
      header={
        <PageHeader
          title="IP pools"
          description="Source addresses for outbound mail; assign pools per server."
          className="mb-0"
          action={
            <Button size="sm" onClick={() => setOpen(true)}>
              <PlusIcon className="size-4" /> New pool
            </Button>
          }
        />
      }
    >
      <div className="space-y-4">
      <ReverseDnsCard />
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
    </Page>
  )
}
