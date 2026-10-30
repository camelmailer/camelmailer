import { describe, expect, it } from "vitest"
import { listed } from "@/views/admin/volume"

describe("listed", () => {
  it("names every entry of a short selection", () => {
    expect(listed(["a", "b", "c"])).toBe("a, b, c")
  })

  it("names the first few of a long selection and counts the rest", () => {
    const names = Array.from({ length: 11 }, (_, i) => `org-${i + 1}`)
    expect(listed(names, 3)).toBe("org-1, org-2, org-3 and 8 more")
  })
})
