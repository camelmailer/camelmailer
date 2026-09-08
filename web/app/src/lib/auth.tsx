"use client"

// Session state: holds the Bearer token + the /me payload and exposes
// login/logout. Pages consume it via useAuth().

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useState,
  useSyncExternalStore,
  type ReactNode,
} from "react"
import { authApi, getToken, setToken, subscribeToken, type MeResponse } from "./api"

type AuthContextValue = {
  token: string | null
  me: MeResponse | null
  loading: boolean
  /** Re-fetch /me (after profile/membership changes). */
  refresh: () => Promise<void>
  /** Store a token obtained from login/SSO/invitation and load /me. */
  adopt: (token: string) => Promise<void>
  logout: () => Promise<void>
}

const AuthContext = createContext<AuthContextValue | null>(null)

export function AuthProvider({ children }: { children: ReactNode }) {
  // Read straight out of localStorage rather than mirrored into state: the
  // server snapshot is null, so server rendering and hydration agree on the
  // initial markup, and every setToken anywhere in the app lands here (a
  // password change issues a fresh session token outside this provider).
  const token = useSyncExternalStore(subscribeToken, getToken, () => null)
  const [me, setMe] = useState<MeResponse | null>(null)
  const [loading, setLoading] = useState<boolean>(true)

  // Fetches /me without touching React state, so the mount effect below can
  // write the result asynchronously instead of synchronously.
  const loadMe = useCallback(async (): Promise<MeResponse | null> => {
    if (!getToken()) return null
    try {
      return await authApi.me()
    } catch {
      // token invalid/expired — drop it
      setToken(null)
      return null
    }
  }, [])

  const refresh = useCallback(async () => {
    setMe(await loadMe())
  }, [loadMe])

  // One /me on mount; loadMe answers null when there is no stored token, so
  // both paths clear `loading` the same way.
  useEffect(() => {
    loadMe()
      .then(setMe)
      .finally(() => setLoading(false))
  }, [loadMe])

  const adopt = useCallback(
    async (newToken: string) => {
      setToken(newToken)
      await refresh()
    },
    [refresh],
  )

  const logout = useCallback(async () => {
    // For a session that came from OIDC the API answers with the provider's
    // end-session URL. Revoking our session alone leaves the provider's
    // standing, so the next sign-in would succeed with no prompt and the
    // user was never really logged out. Navigating there ends it, and the
    // provider returns the browser to /login afterwards.
    let endSessionUrl: string | null = null
    try {
      const result = await authApi.logout()
      endSessionUrl = result.end_session_url ?? null
    } catch {
      // the session may already be gone — that's fine
    }
    setToken(null)
    setMe(null)
    if (endSessionUrl) {
      window.location.assign(endSessionUrl)
    }
  }, [])

  return (
    <AuthContext.Provider value={{ token, me, loading, refresh, adopt, logout }}>
      {children}
    </AuthContext.Provider>
  )
}

export function useAuth(): AuthContextValue {
  const value = useContext(AuthContext)
  if (!value) throw new Error("useAuth must be used inside AuthProvider")
  return value
}
