"use client"

// Recipient detail — one screen that answers "the customer says the
// mail never arrived": a mini reputation (sent/delivered/failed/held),
// and a vertical event history of every message to this address with
// expandable per-delivery SMTP details. Linked from the Messages list
// and the suppression list.

import { useMemo, useState } from "react"
import Link from "next/link"
import { useRouter } from "next/navigation"
import { useQuery } from "@tanstack/react-query"
import {
  ArrowLeftIcon,
  ChevronDownIcon,
  ChevronRightIcon,
  KeyRoundIcon,
  MailIcon,
} from "lucide-react"
import { formatDate } from "@/components/shared"
import { EmptyState } from "@/components/empty-state"
import { MessagePill, messageStatus, statusDotClass } from "@/components/status-pill"
import { Badge } from "@/components/ui/badge"
import { Card, CardContent } from "@/components/ui/card"
import { cn } from "@/lib/utils"
import { serverApi, type Message } from "@/lib/api"
import { useServerMessagingApi } from "@/lib/api-p2"
import { SendMessageButton } from "@/views/server/Messaging"

type Api = ReturnType<typeof serverApi>

// ------------------------------------------------------------- helpers

const failed = (m: Message) =>
  m.status === "Bounced" || m.status === "HardFail" || m.bounce === true

// ------------------------------------------------------------ timeline

/// The delivery attempts of one message, fetched lazily when expanded —
/// raw SMTP responses are the proof of delivery.
function DeliveryDetails({ api, id }: { api: Api; id: number }) {
  const deliveries = useQuery({
    queryKey: ["sapi-deliveries", id],
    queryFn: () => api.deliveries(id),
  })
  if (deliveries.isLoading) {
    return <p className="text-xs text-muted-foreground">Loading deliveries…</p>
  }
  if (!deliveries.data || deliveries.data.deliveries.length === 0) {
    return <p className="text-xs text-muted-foreground">No delivery attempts yet.</p>
  }
  return (
    <div className="grid gap-2">
      {deliveries.data.deliveries.map((delivery) => (
        <div key={delivery.id} className="rounded-md border p-2 text-xs">
          <div className="flex flex-wrap items-center gap-2">
            <MessagePill message={{ status: delivery.status, held: false }} />
            <span className="text-muted-foreground">{formatDate(delivery.timestamp)}</span>
            {delivery.sent_with_ssl && <Badge variant="outline">TLS</Badge>}
          </div>
          {delivery.details && <p className="mt-1.5">{delivery.details}</p>}
          {delivery.output && (
            <pre className="mt-1.5 overflow-x-auto rounded bg-muted p-2 font-mono">
              {delivery.output}
            </pre>
          )}
        </div>
      ))}
    </div>
  )
}

function TimelineItem({
  api,
  message,
  onOpen,
}: {
  api: Api
  message: Message
  onOpen: () => void
}) {
  const [expanded, setExpanded] = useState(false)
  const status = messageStatus(message)
  return (
    <li className="relative pb-6 pl-6 last:pb-0">
      {/* tone-colored dot on the timeline rail */}
      <span
        className={cn(
          "absolute left-0 top-1.5 size-2.5 -translate-x-1/2 rounded-full ring-4 ring-background",
          statusDotClass(status),
        )}
        aria-hidden
      />
      <div className="flex flex-wrap items-center gap-2">
        <MessagePill message={message} />
        <button
          type="button"
          onClick={onOpen}
          className="text-left text-sm font-medium hover:underline"
        >
          {message.subject ?? `Message #${message.id}`}
        </button>
        <span className="text-xs text-muted-foreground">{formatDate(message.created_at)}</span>
      </div>
      <button
        type="button"
        onClick={() => setExpanded((value) => !value)}
        className="mt-1 inline-flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
      >
        {expanded ? (
          <ChevronDownIcon className="size-3.5" />
        ) : (
          <ChevronRightIcon className="size-3.5" />
        )}
        Deliveries
      </button>
      {expanded && (
        <div className="mt-2">
          <DeliveryDetails api={api} id={message.id} />
        </div>
      )}
    </li>
  )
}

// ---------------------------------------------------------------- view

export function RecipientDetail({
  org,
  server,
  email,
}: {
  org: string
  server: string
  email: string
}) {
  const { api, isLoading } = useServerMessagingApi(org, server)
  const router = useRouter()

  const messagesQuery = useQuery({
    queryKey: ["recipient-messages", org, server, email],
    // substring match on subject/addresses — narrowed to the exact
    // recipient below
    queryFn: () =>
      api!.messages(`?scope=outgoing&per_page=100&query=${encodeURIComponent(email)}`),
    enabled: api !== null,
  })
  const messages = useMemo(
    () =>
      (messagesQuery.data?.messages ?? []).filter(
        (message) => message.rcpt_to.toLowerCase() === email.toLowerCase(),
      ),
    [messagesQuery.data, email],
  )

  const kpis: [string, number][] = [
    ["Messages", messages.length],
    ["Delivered", messages.filter((m) => m.status === "Sent" && !m.held).length],
    ["Failed", messages.filter(failed).length],
    ["Held", messages.filter((m) => m.held).length],
  ]

  return (
    <div className="space-y-4">
      <Link
        href={`/orgs/${org}/servers/${server}/messaging`}
        className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
      >
        <ArrowLeftIcon className="size-3.5" /> All messages
      </Link>

      <div className="flex items-center gap-3">
        <div className="flex size-10 shrink-0 items-center justify-center rounded-full bg-muted text-base font-semibold uppercase">
          {email.charAt(0)}
        </div>
        <div className="min-w-0">
          <h1 className="truncate text-lg font-semibold">{email}</h1>
          <p className="text-sm text-muted-foreground">
            Delivery history on this server (last {messagesQuery.data ? 100 : "…"} messages)
          </p>
        </div>
      </div>

      {!api && !isLoading ? (
        <EmptyState
          icon={KeyRoundIcon}
          title="Connect an API credential"
          description="Recipient history talks to the server's own API. Create an API credential first, then come back here."
          action={{
            label: "Create API credential",
            href: `/orgs/${org}/servers/${server}/credentials`,
          }}
        />
      ) : (
        <>
          <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
            {kpis.map(([label, value]) => (
              <Card key={label}>
                <CardContent className="p-4">
                  <p className="text-xs text-muted-foreground">{label}</p>
                  <p className="text-2xl font-semibold tabular-nums">
                    {messagesQuery.data ? value : "—"}
                  </p>
                </CardContent>
              </Card>
            ))}
          </div>

          {messagesQuery.isLoading || isLoading ? (
            <p className="text-sm text-muted-foreground">Loading history…</p>
          ) : messages.length === 0 ? (
            <EmptyState
              icon={MailIcon}
              title="No messages to this address"
              description="Nothing in the recent history of this server was sent to this recipient."
            >
              <SendMessageButton org={org} server={server} variant="outline" defaultTo={email} />
            </EmptyState>
          ) : (
            api && (
              <ol className="relative ml-1.5 border-l">
                {messages.map((message) => (
                  <TimelineItem
                    key={message.id}
                    api={api}
                    message={message}
                    onOpen={() =>
                      router.push(`/orgs/${org}/servers/${server}/messaging/${message.id}`)
                    }
                  />
                ))}
              </ol>
            )
          )}
        </>
      )}
    </div>
  )
}
