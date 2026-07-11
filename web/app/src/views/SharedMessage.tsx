"use client"

// The public, read-only message view behind a share link
// (`/share/m/<token>`). No login, no app shell — everything the support
// counterpart needs to see: metadata, the delivery timeline (attempts +
// opens + clicks) and the message content as HTML preview / plain text.

import { useQuery } from "@tanstack/react-query"
import { formatDate } from "@/components/shared"
import { Badge } from "@/components/ui/badge"
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { ApiError, shareApi, type SharedMessage as SharedMessageData } from "@/lib/api"

function statusBadge(status: string | null | undefined) {
  switch (status) {
    case "Sent":
      return <Badge>sent</Badge>
    case "SoftFail":
      return <Badge variant="secondary">soft fail</Badge>
    case "HardFail":
    case "Bounced":
      return <Badge variant="destructive">{status.toLowerCase()}</Badge>
    default:
      return <Badge variant="outline">{status ?? "pending"}</Badge>
  }
}

/// The deliveries, opens and clicks folded into one chronological timeline.
function timelineOf(data: SharedMessageData) {
  const events: { at: string; label: string; detail: string }[] = []
  for (const delivery of data.deliveries) {
    events.push({
      at: delivery.timestamp ?? "",
      label: `Delivery: ${delivery.status}`,
      detail: delivery.details ?? delivery.output ?? "",
    })
  }
  for (const open of data.opens) {
    events.push({
      at: open.created_at,
      label: "Opened",
      detail: [open.ip_address, open.user_agent].filter(Boolean).join(" · "),
    })
  }
  for (const click of data.clicks) {
    events.push({
      at: click.created_at,
      label: "Clicked",
      detail: [click.url, click.ip_address].filter(Boolean).join(" · "),
    })
  }
  return events.sort((a, b) => a.at.localeCompare(b.at))
}

export default function SharedMessage({ token }: { token: string }) {
  const shared = useQuery({
    queryKey: ["shared-message", token],
    queryFn: () => shareApi.message(token),
    retry: false,
  })

  if (shared.isLoading) {
    return (
      <main className="mx-auto max-w-3xl p-6">
        <p className="text-sm text-muted-foreground">Loading…</p>
      </main>
    )
  }
  if (shared.isError || !shared.data) {
    const expired =
      shared.error instanceof ApiError && shared.error.code === "ShareLinkExpired"
    return (
      <main className="mx-auto max-w-3xl p-6">
        <Card>
          <CardHeader>
            <CardTitle>{expired ? "This share link has expired" : "Link not found"}</CardTitle>
          </CardHeader>
          <CardContent className="text-sm text-muted-foreground">
            {expired
              ? "Ask the sender to generate a fresh link."
              : "This share link does not exist (or has been removed)."}
          </CardContent>
        </Card>
      </main>
    )
  }

  const data = shared.data
  const m = data.message
  const timeline = timelineOf(data)

  return (
    <main className="mx-auto max-w-3xl space-y-6 p-6">
      <header className="flex items-start justify-between gap-4">
        <div>
          <h1 className="text-lg font-semibold">Shared message</h1>
          <p className="text-sm text-muted-foreground">
            Read-only view · link expires {formatDate(data.expires_at)}
          </p>
        </div>
        {statusBadge(m.status)}
      </header>

      <Card>
        <CardContent className="grid grid-cols-[7rem_1fr] gap-1 p-4 text-sm">
          <span className="text-muted-foreground">From</span>
          <span className="break-all">{m.mail_from ?? "—"}</span>
          <span className="text-muted-foreground">To</span>
          <span className="break-all">{m.rcpt_to}</span>
          <span className="text-muted-foreground">Subject</span>
          <span>{m.subject ?? "—"}</span>
          <span className="text-muted-foreground">Created</span>
          <span>{formatDate(m.created_at)}</span>
          <span className="text-muted-foreground">Spam</span>
          <span>
            {m.spam_status ?? "—"}
            {m.spam_score != null && ` (${m.spam_score})`}
          </span>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">Timeline</CardTitle>
        </CardHeader>
        <CardContent>
          {timeline.length === 0 ? (
            <p className="text-sm text-muted-foreground">No events recorded yet.</p>
          ) : (
            <ol className="space-y-2">
              {timeline.map((event, index) => (
                <li key={index} className="grid grid-cols-[10rem_1fr] gap-2 text-sm">
                  <span className="whitespace-nowrap text-muted-foreground">
                    {formatDate(event.at)}
                  </span>
                  <span>
                    <span className="font-medium">{event.label}</span>
                    {event.detail && (
                      <span className="block break-all text-xs text-muted-foreground">
                        {event.detail}
                      </span>
                    )}
                  </span>
                </li>
              ))}
            </ol>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">Content</CardTitle>
        </CardHeader>
        <CardContent>
          <Tabs defaultValue={data.html_body ? "preview" : "text"}>
            <TabsList className="mb-2">
              <TabsTrigger value="preview" disabled={!data.html_body}>
                Preview
              </TabsTrigger>
              <TabsTrigger value="text" disabled={!data.text_body}>
                Text
              </TabsTrigger>
            </TabsList>
            <TabsContent value="preview">
              {data.html_body ? (
                <iframe
                  title="Message preview"
                  sandbox=""
                  srcDoc={data.html_body}
                  className="h-96 w-full rounded-md border bg-white"
                />
              ) : (
                <p className="text-sm text-muted-foreground">No HTML part.</p>
              )}
            </TabsContent>
            <TabsContent value="text">
              {data.text_body ? (
                <pre className="max-h-96 overflow-auto whitespace-pre-wrap rounded-md bg-muted p-3 text-xs">
                  {data.text_body}
                </pre>
              ) : (
                <p className="text-sm text-muted-foreground">No plain-text part.</p>
              )}
            </TabsContent>
          </Tabs>
        </CardContent>
      </Card>

      <footer className="text-center text-xs text-muted-foreground">
        Shared via CamelMailer
      </footer>
    </main>
  )
}
