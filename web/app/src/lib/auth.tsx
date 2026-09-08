"use client"

// Session state: holds the Bearer token + the /me payload and exposes
// login/logout. Pages consume it via useAuth().

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useState,
  type ReactNode,
} from "react"
import { authApi, getToken, setToken, type MeResponse } from "./api"

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
  // Initialized in an effect (not from localStorage directly) so server
  // rendering and hydration agree on the initial markup.
  const [token, setTokenState] = useState<string | null>(null)
  const [me, setMe] = useState<MeResponse | null>(null)
  const [loading, setLoading] = useState<boolean>(true)

  const refresh = useCallback(async () => {
    if (!getToken()) {
      setMe(null)
      return
    }
    try {
      setMe(await authApi.me())
    } catch {
      // token invalid/expired — drop it
      setToken(null)
      setTokenState(null)
      setMe(null)
    }
  }, [])

  useEffect(() => {
    const stored = getToken()
    setTokenState(stored)
    if (stored) {
      refresh().finally(() => setLoading(false))
    } else {
      setLoading(false)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  const adopt = useCallback(
    async (newToken: string) => {
      setToken(newToken)
      setTokenState(newToken)
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
    setTokenState(null)
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
