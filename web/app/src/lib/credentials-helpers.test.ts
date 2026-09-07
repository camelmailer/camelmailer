import { describe, it, expect, vi } from "vitest"
import messageBouncedSampleShape from "./__fixtures__/message_bounced_sample_shape.json"
import type { Domain } from "@/lib/api"
import { maskKey, deriveSmtpHost, webhookSamplePayload } from "@/lib/api-p3"
import { relativeTime } from "@/lib/api-p1"

// Regression tests for the credentials-page crash: opening
// /orgs/<org>/servers/<server>/credentials threw
// "Cannot read properties of undefined (reading 'length')" because
// `maskKey(credential.key)` assumed `key` was always a present, non-empty
// string. These helpers must now tolerate missing/empty/undefined input
// and never throw, so the page renders even when a field is absent or a
// dependent query (domains) has not loaded yet.

describe("maskKey", () => {
  it("returns '' for an empty key without throwing", () => {
    expect(maskKey("")).toBe("")
  })

  it("tolerates undefined (a listed credential may lack its secret key)", () => {
    expect(() => maskKey(undefined as unknown as string)).not.toThrow()
    expect(maskKey(undefined as unknown as string)).toBe("")
  })

  it("tolerates null", () => {
    expect(() => maskKey(null as unknown as string)).not.toThrow()
    expect(maskKey(null as unknown as string)).toBe("")
  })

  it("masks a real key, keeping the prefix and last four", () => {
    expect(maskKey("cm_abcdef1234")).toBe("cm_••••••1234")
  })

  it("masks a prefixless key", () => {
    const masked = maskKey("abcdef1234")
    expect(masked.endsWith("1234")).toBe(true)
    expect(masked.startsWith("•")).toBe(true)
  })
})

describe("deriveSmtpHost", () => {
  it("falls back to the platform host when domains are undefined", () => {
    expect(deriveSmtpHost(undefined, "app.camelmailer.com")).toBe("smtp.camelmailer.com")
  })

  it("falls back when domains have not loaded (null)", () => {
    expect(deriveSmtpHost(null, "app.camelmailer.com")).toBe("smtp.camelmailer.com")
  })

  it("returns the platform host unchanged when it has no app. prefix", () => {
    expect(deriveSmtpHost([], "mail.example.com")).toBe("mail.example.com")
  })

  it("prefers the smtp_hostname embedded in a domain's SPF record", () => {
    const domains = [
      { spf_record: { value: "v=spf1 a:mx.x.com -all" } },
    ] as unknown as Domain[]
    expect(deriveSmtpHost(domains, "app.camelmailer.com")).toBe("mx.x.com")
  })

  it("ignores a domain whose SPF record is missing and uses the fallback", () => {
    const domains = [{}, { spf_record: null }] as unknown as Domain[]
    expect(deriveSmtpHost(domains, "app.camelmailer.com")).toBe("smtp.camelmailer.com")
  })

  it("tolerates a nullish fallback host", () => {
    expect(() => deriveSmtpHost(undefined, undefined)).not.toThrow()
    expect(deriveSmtpHost(undefined, null)).toBe("")
  })
})

describe("relativeTime", () => {
  it("returns a placeholder for undefined without throwing", () => {
    expect(() => relativeTime(undefined)).not.toThrow()
    expect(relativeTime(undefined)).toBe("—")
  })

  it("returns a placeholder for null without throwing", () => {
    expect(relativeTime(null)).toBe("—")
  })

  it("echoes an unparseable value rather than throwing", () => {
    expect(relativeTime("not-a-date")).toBe("not-a-date")
  })

  it("formats a very recent timestamp as 'just now'", () => {
    expect(relativeTime(new Date().toISOString())).toBe("just now")
  })
})

describe("webhookSamplePayload", () => {
  it("uses fractional-second timestamps for MessageBounced samples", () => {
    vi.spyOn(Date, "now").mockReturnValue(1_720_000_000_123)
    const sample = JSON.parse(webhookSamplePayload("MessageBounced"))

    expect(sample.payload.bounce.timestamp).toBe(1_720_000_000.123)
    expect(sample.payload.original_message.timestamp).toBe(1_719_999_940.123)
    expect(Number.isInteger(sample.payload.bounce.timestamp)).toBe(false)

    vi.restoreAllMocks()
  })

  it("matches the backend MessageBounced sample field contract", () => {
    const sample = JSON.parse(webhookSamplePayload("MessageBounced"))
    const keys = (value: object) => Object.keys(value).sort()

    expect(keys(sample)).toEqual(messageBouncedSampleShape.top_level)
    expect(keys(sample.payload)).toEqual(messageBouncedSampleShape.payload)
    expect(keys(sample.payload.original_message)).toEqual(
      messageBouncedSampleShape.message,
    )
    expect(keys(sample.payload.bounce)).toEqual(messageBouncedSampleShape.message)
    expect(sample.payload.original_message.message_id).toBe(
      "original@example.com",
    )
    expect(sample.payload.bounce.message_id).toBe("bounce@mx.example.com")
  })
})
