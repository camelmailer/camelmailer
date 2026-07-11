import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { BrowserRouter, Navigate, Outlet, Route, Routes } from "react-router-dom"
import { Toaster } from "@/components/ui/sonner"
import AppShell from "@/layouts/AppShell"
import { AuthProvider, useAuth } from "@/lib/auth"
import AcceptInvitation from "@/pages/auth/AcceptInvitation"
import Login from "@/pages/auth/Login"
import OidcCallback from "@/pages/auth/OidcCallback"
import ResetPassword from "@/pages/auth/ResetPassword"
import Account from "@/pages/account/Account"
import AdminApiKeys from "@/pages/admin/AdminApiKeys"
import AuditLog from "@/pages/admin/AuditLog"
import IpPools from "@/pages/admin/IpPools"
import Users from "@/pages/admin/Users"
import Dashboard from "@/pages/Dashboard"
import OrgHome from "@/pages/org/OrgHome"
import ServerHome from "@/pages/server/ServerHome"

const queryClient = new QueryClient({
  defaultOptions: { queries: { retry: 1, refetchOnWindowFocus: false } },
})

function RequireAuth() {
  const { token, loading } = useAuth()
  if (loading) {
    return <div className="p-8 text-sm text-muted-foreground">Loading…</div>
  }
  if (!token) return <Navigate to="/login" replace />
  return <Outlet />
}

export default function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <AuthProvider>
        <BrowserRouter>
          <Routes>
            <Route path="/login" element={<Login />} />
            <Route path="/forgot-password" element={<ResetPassword />} />
            <Route path="/reset-password" element={<ResetPassword />} />
            <Route path="/invitations/accept" element={<AcceptInvitation />} />
            <Route path="/auth/callback" element={<OidcCallback />} />
            <Route element={<RequireAuth />}>
              <Route element={<AppShell />}>
                <Route path="/" element={<Dashboard />} />
                <Route path="/orgs" element={<Dashboard all />} />
                <Route path="/orgs/:org/*" element={<OrgHome />} />
                <Route path="/orgs/:org/servers/:server/*" element={<ServerHome />} />
                <Route path="/account" element={<Account />} />
                <Route path="/admin/users" element={<Users />} />
                <Route path="/admin/ip-pools" element={<IpPools />} />
                <Route path="/admin/api-keys" element={<AdminApiKeys />} />
                <Route path="/admin/audit" element={<AuditLog />} />
              </Route>
            </Route>
            <Route path="*" element={<Navigate to="/" replace />} />
          </Routes>
        </BrowserRouter>
        <Toaster />
      </AuthProvider>
    </QueryClientProvider>
  )
}
