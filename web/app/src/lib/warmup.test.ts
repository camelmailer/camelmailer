import { describe, expect, it } from "vitest"
import {
  etaText,
  providerLabel,
  formatRate,
  lastBrake,
  rateLevel,
  selectorValue,
  usedPercent,
  warmupBody,
  type WarmupAddress,
} from "./warmup"

function address(overrides: Partial<WarmupAddress> = {}): WarmupAddress {
  return {
    ip_address_id: 1,
    ipv4: "203.0.113.5",
    hostname: "out1.example.com",
    pool: { id: 1, name: "Main", mode: null },
    mode: "off",
    mode_source: "default",
    address_mode: null,
    active: false,
    profile: null,
    providers: [],
    eta_days: null,
    recent_decisions: [],
    ...overrides,
  }
}

const provider = {
  allowance: 200,
  used_today: 50,
  status: "ramping" as const,
  paused: false,
  paused_until: null,
  last_reason: null,
  last_brake_reason: null,
  last_brake_at: null,
  evaluated_day: null,
  progress: 0.004,
  eta_days: 14,
}

describe("warm-up helpers", () => {
  it("labels built-in and configured provider groups", () => {
    expect(providerLabel("united_internet")).toBe("United Internet")
    expect(providerLabel("t_online")).toBe("T-Online")
    expect(providerLabel("seznam_cz")).toBe("seznam cz")
  })

  it("formats rates and levels", () => {
    expect(formatRate(null)).toBe("—")
    expect(formatRate(0.0512)).toBe("5.12 %")
    expect(rateLevel(0.0005, 0.001, 0.003)).toBe("ok")
    expect(rateLevel(0.0015, 0.001, 0.003)).toBe("warn")
    expect(rateLevel(0.003, 0.001, 0.003)).toBe("bad")
    expect(rateLevel(null, 0.001, 0.003)).toBe("ok")
  })

  it("estimates and progress", () => {
    expect(etaText({ eta_days: 14, status: "ramping" })).toBe("14 days")
    expect(etaText({ eta_days: 1, status: "holding" })).toBe("1 day")
    expect(etaText({ eta_days: 3, status: "warm" })).toBe("warm")
    expect(etaText({ eta_days: null, status: null })).toBe("—")
    expect(usedPercent({ used_today: 50, allowance: 200 })).toBe(25)
    expect(usedPercent({ used_today: 500, allowance: 200 })).toBe(100)
    expect(usedPercent({ used_today: 5, allowance: null })).toBe(0)
  })

  it("builds PATCH bodies and refuses incomplete custom profiles", () => {
    const empty = { start: "", growth: "", cap: "" }
    expect(warmupBody("slow", empty)).toEqual({ mode: "slow" })
    expect(warmupBody("inherit", empty)).toEqual({ mode: "inherit" })
    expect(warmupBody("custom", empty)).toBeNull()
    expect(warmupBody("custom", { start: "100", growth: "1.4", cap: "50" })).toBeNull()
    expect(warmupBody("custom", { start: "100", growth: "4", cap: "5000" })).toBeNull()
    expect(warmupBody("custom", { start: "100", growth: "1.4", cap: "5000" })).toEqual({
      mode: "custom",
      profile: { start: 100, growth: 1.4, cap: 5000 },
    })
  })

  it("shows the address's own mode or inherit", () => {
    expect(selectorValue(address())).toBe("inherit")
    expect(selectorValue(address({ address_mode: "fast" }))).toBe("fast")
  })

  it("finds the latest brake over all providers", () => {
    expect(lastBrake(address())).toBeNull()
    const braked = address({
      providers: [
        { ...provider, provider: "google", last_brake_reason: "old", last_brake_at: "2026-11-10T00:00:00Z" },
        { ...provider, provider: "microsoft", last_brake_reason: "S3150", last_brake_at: "2026-11-14T00:00:00Z" },
      ],
    })
    expect(lastBrake(braked)).toEqual({ provider: "microsoft", reason: "S3150", at: "2026-11-14T00:00:00Z" })
  })
})
