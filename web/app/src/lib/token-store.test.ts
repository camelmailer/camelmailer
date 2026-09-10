import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"

// The session token lives in localStorage, and AuthProvider reads it through
// useSyncExternalStore, so every write has to reach the subscribers: a stale
// mirror is what let a second tab keep showing a signed-in shell after the
// session was revoked. These tests stand in for the browser globals rather
// than pulling in a DOM environment, which is all the store touches.

type StorageEvents = Record<string, Set<(event: unknown) => void>>

let store: Record<string, string>
let events: StorageEvents

beforeEach(() => {
  store = {}
  events = {}
  Object.assign(globalThis, {
    localStorage: {
      getItem: (key: string) => (key in store ? store[key] : null),
      setItem: (key: string, value: string) => {
        store[key] = value
      },
      removeItem: (key: string) => {
        delete store[key]
      },
    },
    window: {
      addEventListener: (type: string, listener: (event: unknown) => void) => {
        ;(events[type] ??= new Set()).add(listener)
      },
      removeEventListener: (type: string, listener: (event: unknown) => void) => {
        events[type]?.delete(listener)
      },
    },
  })
})

afterEach(() => {
  vi.resetModules()
  delete (globalThis as Record<string, unknown>).localStorage
  delete (globalThis as Record<string, unknown>).window
})

async function tokenStore() {
  return await import("./api")
}

describe("session token store", () => {
  it("reads back what was written", async () => {
    const { getToken, setToken } = await tokenStore()
    expect(getToken()).toBeNull()
    setToken("abc")
    expect(getToken()).toBe("abc")
    setToken(null)
    expect(getToken()).toBeNull()
  })

  it("notifies subscribers on a write in this tab", async () => {
    const { setToken, subscribeToken } = await tokenStore()
    const seen: (string | null)[] = []
    const { getToken } = await tokenStore()
    const unsubscribe = subscribeToken(() => seen.push(getToken()))

    setToken("first")
    setToken("second")
    setToken(null)

    expect(seen).toEqual(["first", "second", null])
    unsubscribe()
  })

  it("stops notifying once unsubscribed", async () => {
    const { setToken, subscribeToken } = await tokenStore()
    const listener = vi.fn()
    subscribeToken(listener)()

    setToken("abc")

    expect(listener).not.toHaveBeenCalled()
  })

  it("notifies on a storage event from another tab", async () => {
    const { subscribeToken } = await tokenStore()
    const listener = vi.fn()
    const unsubscribe = subscribeToken(listener)

    for (const handler of events.storage ?? []) handler({})

    expect(listener).toHaveBeenCalledTimes(1)
    unsubscribe()
    expect(events.storage?.size ?? 0).toBe(0)
  })
})
