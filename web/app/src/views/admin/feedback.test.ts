import { describe, expect, it } from "vitest"
import type { FeedbackSource } from "@/lib/api"
import {
  age,
  authSummary,
  formatMetric,
  placementTone,
  sourceState,
  verdictTone,
  worstVerdict,
} from "@/views/admin/feedback"

function source(overrides: Partial<FeedbackSource>): FeedbackSource {
  return {
    source: "snds",
    provider: "microsoft",
    enabled: true,
    freshness_seconds: 3600,
    status: null,
    snapshots: [],
    ...overrides,
  }
}

describe("sourceState", () => {
  it("is off without credentials and failing after an error", () => {
    expect(sourceState(source({ enabled: false }))).toBe("off")
    expect(
      sourceState(
        source({
          status: {
            source: "snds",
            enabled: true,
            last_attempt_at: null,
            last_success_at: null,
            last_error: "HTTP 500",
            consecutive_failures: 2,
            next_attempt_at: null,
          },
        }),
      ),
    ).toBe("failing")
  })

  it("calls fetched data older than two days stale, but never a quiet feedback loop", () => {
    expect(sourceState(source({ freshness_seconds: 200_000 }))).toBe("stale")
    expect(sourceState(source({ source: "fbl", freshness_seconds: 900_000 }))).toBe("fresh")
    expect(sourceState(source({ freshness_seconds: null }))).toBe("waiting")
  })
})

describe("verdicts and values", () => {
  it("picks the worst verdict and colours it", () => {
    const snapshot = (verdict: "ok" | "warn" | "bad") => ({
      source: "snds",
      provider: "microsoft" as const,
      subject_kind: "ip" as const,
      subject: "192.0.2.1",
      data_date: "2026-11-12",
      verdict,
      detail: "",
      metrics: {},
      fetched_at: "2026-11-13T00:00:00Z",
    })
    expect(worstVerdict(source({ snapshots: [snapshot("ok"), snapshot("bad"), snapshot("warn")] }))).toBe(
      "bad",
    )
    expect(worstVerdict(source({}))).toBe("unknown")
    expect(verdictTone("warn")).toBe("amber")
  })

  it("shows rates as percentages and ages in friendly units", () => {
    expect(formatMetric("spam_rate", 0.0015)).toBe("0.15 %")
    expect(formatMetric("trap_hits", 3)).toBe("3")
    expect(formatMetric("x", null)).toBe("—")
    expect(age(90)).toBe("2 min")
    expect(age(7200)).toBe("2 h")
    expect(age(400_000)).toBe("5 d")
    expect(age(null)).toBeNull()
  })
})

describe("placement", () => {
  it("names results and summarises authentication", () => {
    expect(placementTone("spam")).toEqual({ label: "Spam", tone: "red" })
    expect(placementTone("pending").label).toBe("Waiting")
    expect(authSummary({ spf: "pass", dkim: null, dmarc: "fail" })).toBe("spf pass · dmarc fail")
    expect(authSummary({ spf: null, dkim: null, dmarc: null })).toBeNull()
  })

  it("never calls placement stale", () => {
    expect(sourceState(source({ source: "placement", freshness_seconds: 900_000 }))).toBe("fresh")
  })
})
