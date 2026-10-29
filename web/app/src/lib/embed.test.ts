import { describe, expect, it } from "vitest"
import { embedPlan, isCustomElementName, safeAttributes } from "@/lib/embed"
import type { EmbedSession } from "@/lib/api"

const base: EmbedSession = {
  script_url: "https://billing.example/elements.js",
  element: "rs-checkout",
  token: "tok_1",
  attributes: {},
  url: "https://billing.example/checkout/1",
  expires_at: null,
}

describe("embedPlan", () => {
  it("renders the provider's element with its token and keeps the link as fallback", () => {
    expect(embedPlan(base)).toEqual({
      kind: "element",
      element: "rs-checkout",
      scriptUrl: "https://billing.example/elements.js",
      attributes: { token: "tok_1" },
      companions: [],
      fallbackUrl: "https://billing.example/checkout/1",
    })
  })

  it("renders companions with the same token and drops unsafe ones", () => {
    const plan = embedPlan({
      ...base,
      element: "rs-subscription",
      companions: [
        { element: "rs-invoices" },
        { element: "rs-usage", attributes: { metric: "emails", token: "other", onload: "x" } },
        { element: "iframe", attributes: {} },
      ],
    })
    expect(plan.kind === "element" && plan.companions).toEqual([
      { element: "rs-invoices", attributes: { token: "tok_1" } },
      { element: "rs-usage", attributes: { metric: "emails", token: "tok_1" } },
    ])
  })

  it("falls back to the link without a usable element or script", () => {
    expect(embedPlan({ ...base, element: null })).toEqual({
      kind: "link",
      url: "https://billing.example/checkout/1",
    })
    expect(embedPlan({ ...base, script_url: "javascript:alert(1)" })).toEqual({
      kind: "link",
      url: "https://billing.example/checkout/1",
    })
  })

  it("shows nothing for an empty or unsafe session", () => {
    expect(embedPlan(null)).toEqual({ kind: "none" })
    expect(embedPlan({ ...base, element: "script", url: "javascript:x" })).toEqual({
      kind: "none",
    })
  })
})

describe("element and attribute safety", () => {
  it("accepts only custom element names", () => {
    expect(isCustomElementName("rs-usage")).toBe(true)
    expect(isCustomElementName("iframe")).toBe(false)
    expect(isCustomElementName("Rs-Usage")).toBe(false)
    expect(isCustomElementName("rs-x><script")).toBe(false)
  })

  it("drops event handlers and odd names, and never lets attributes override the token", () => {
    expect(
      safeAttributes({ locale: "de", onclick: "x", "data-x": "1", "a b": "2", token: "t" }),
    ).toEqual({ locale: "de", "data-x": "1" })
  })
})
