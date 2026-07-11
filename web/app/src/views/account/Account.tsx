"use client"

// Account & security: profile, password change, TOTP enrollment,
// passkeys (WebAuthn).

import { useEffect, useState } from "react"
import { QRCodeSVG } from "qrcode.react"
import { toast } from "sonner"
import { PageHeader, SecretReveal } from "@/components/shared"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { ApiError, authApi, setToken, type PasskeyCredential } from "@/lib/api"
import { useAuth } from "@/lib/auth"
import { createPasskey, webAuthnSupported, type CreationOptionsJSON } from "@/lib/webauthn"

function errorToast(err: unknown, fallback: string) {
  toast.error(err instanceof ApiError ? err.message : fallback)
}

export default function Account() {
  const { me, refresh } = useAuth()
  const [firstName, setFirstName] = useState(me?.user.first_name ?? "")
  const [lastName, setLastName] = useState(me?.user.last_name ?? "")
  const [currentPassword, setCurrentPassword] = useState("")
  const [newPassword, setNewPassword] = useState("")

  // TOTP enrollment state
  const [enroll, setEnroll] = useState<{ secret: string; otpauth_url: string } | null>(null)
  const [totpCode, setTotpCode] = useState("")
  const [disableOpen, setDisableOpen] = useState(false)
  const [disablePassword, setDisablePassword] = useState("")

  // Passkeys (only shown when the instance has auth.webauthn enabled)
  const [webauthnEnabled, setWebauthnEnabled] = useState(false)
  const [passkeys, setPasskeys] = useState<PasskeyCredential[]>([])
  const [passkeyName, setPasskeyName] = useState("")
  const [passkeyBusy, setPasskeyBusy] = useState(false)

  const totpEnabled = me?.user.totp_enabled ?? false

  useEffect(() => {
    authApi
      .features()
      .then((features) => {
        setWebauthnEnabled(features.webauthn)
        if (features.webauthn) return loadPasskeys()
      })
      .catch(() => setWebauthnEnabled(false))
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  async function loadPasskeys() {
    try {
      const data = await authApi.webauthnCredentials()
      setPasskeys(data.credentials)
    } catch {
      setPasskeys([])
    }
  }

  async function addPasskey() {
    if (!webAuthnSupported()) {
      toast.error("This browser does not support passkeys")
      return
    }
    setPasskeyBusy(true)
    try {
      const options = await authApi.webauthnRegisterStart()
      const credential = await createPasskey(options as unknown as CreationOptionsJSON)
      await authApi.webauthnRegisterFinish(passkeyName.trim(), credential)
      setPasskeyName("")
      await loadPasskeys()
      toast.success("Passkey added")
    } catch (err) {
      errorToast(err, "Could not add the passkey")
    } finally {
      setPasskeyBusy(false)
    }
  }

  async function deletePasskey(id: number) {
    try {
      await authApi.webauthnDeleteCredential(id)
      await loadPasskeys()
      toast.success("Passkey removed")
    } catch (err) {
      errorToast(err, "Could not remove the passkey")
    }
  }

  async function saveProfile() {
    try {
      await authApi.updateMe({ first_name: firstName, last_name: lastName })
      await refresh()
      toast.success("Profile updated")
    } catch (err) {
      errorToast(err, "Could not update the profile")
    }
  }

  async function changePassword() {
    try {
      const result = await authApi.changePassword(currentPassword, newPassword)
      // all sessions were rotated; adopt the fresh token transparently
      setToken(result.session_token)
      setCurrentPassword("")
      setNewPassword("")
      toast.success("Password changed — other sessions were signed out")
    } catch (err) {
      errorToast(err, "Could not change the password")
    }
  }

  return (
    <div className="max-w-xl space-y-6">
      <PageHeader title="Account & security" />

      <Card>
        <CardHeader>
          <CardTitle className="text-base">Profile</CardTitle>
          <CardDescription>{me?.user.email_address}</CardDescription>
        </CardHeader>
        <CardContent className="grid gap-4">
          <div className="grid grid-cols-2 gap-2">
            <div className="grid gap-2">
              <Label>First name</Label>
              <Input value={firstName} onChange={(e) => setFirstName(e.target.value)} />
            </div>
            <div className="grid gap-2">
              <Label>Last name</Label>
              <Input value={lastName} onChange={(e) => setLastName(e.target.value)} />
            </div>
          </div>
          <Button className="justify-self-start" onClick={saveProfile}>
            Save
          </Button>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">Change password</CardTitle>
          <CardDescription>Changing the password signs out every other session.</CardDescription>
        </CardHeader>
        <CardContent className="grid gap-4">
          <div className="grid gap-2">
            <Label>Current password</Label>
            <Input
              type="password"
              autoComplete="current-password"
              value={currentPassword}
              onChange={(e) => setCurrentPassword(e.target.value)}
            />
          </div>
          <div className="grid gap-2">
            <Label>New password</Label>
            <Input
              type="password"
              autoComplete="new-password"
              minLength={8}
              value={newPassword}
              onChange={(e) => setNewPassword(e.target.value)}
            />
          </div>
          <Button
            className="justify-self-start"
            onClick={changePassword}
            disabled={!currentPassword || newPassword.length < 8}
          >
            Change password
          </Button>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2 text-base">
            Two-factor authentication
            {totpEnabled ? <Badge>enabled</Badge> : <Badge variant="secondary">off</Badge>}
          </CardTitle>
          <CardDescription>
            Time-based codes (TOTP) from Google Authenticator, 1Password &amp; co.
          </CardDescription>
        </CardHeader>
        <CardContent className="grid gap-4">
          {!totpEnabled && !enroll && (
            <Button
              className="justify-self-start"
              onClick={async () => {
                try {
                  setEnroll(await authApi.totpEnroll())
                } catch (err) {
                  errorToast(err, "Could not start enrollment")
                }
              }}
            >
              Set up 2FA
            </Button>
          )}
          {enroll && (
            <div className="grid gap-4">
              <div className="flex items-start gap-4">
                <div className="rounded-lg border bg-white p-2">
                  <QRCodeSVG value={enroll.otpauth_url} size={144} />
                </div>
                <div className="min-w-0 flex-1">
                  <SecretReveal label="Secret (manual entry)" value={enroll.secret} />
                </div>
              </div>
              <div className="grid gap-2">
                <Label>Confirm with a code from your app</Label>
                <div className="flex gap-2">
                  <Input
                    className="w-32"
                    inputMode="numeric"
                    placeholder="123456"
                    value={totpCode}
                    onChange={(e) => setTotpCode(e.target.value)}
                  />
                  <Button
                    onClick={async () => {
                      try {
                        await authApi.totpActivate(totpCode)
                        setEnroll(null)
                        setTotpCode("")
                        await refresh()
                        toast.success("Two-factor authentication is on")
                      } catch (err) {
                        errorToast(err, "The code is incorrect")
                      }
                    }}
                    disabled={totpCode.length !== 6}
                  >
                    Activate
                  </Button>
                </div>
              </div>
            </div>
          )}
          {totpEnabled && (
            <Button
              variant="outline"
              className="justify-self-start"
              onClick={() => setDisableOpen(true)}
            >
              Disable 2FA
            </Button>
          )}
        </CardContent>
      </Card>

      {webauthnEnabled && (
        <Card>
          <CardHeader>
            <CardTitle className="text-base">Passkeys</CardTitle>
            <CardDescription>
              Sign in with Touch ID, Windows Hello or a security key instead of
              your password.
            </CardDescription>
          </CardHeader>
          <CardContent className="grid gap-4">
            {passkeys.length > 0 && (
              <ul className="grid gap-2">
                {passkeys.map((passkey) => (
                  <li
                    key={passkey.id}
                    className="flex items-center justify-between rounded-md border px-3 py-2"
                  >
                    <div className="min-w-0">
                      <div className="truncate text-sm font-medium">{passkey.name}</div>
                      <div className="text-xs text-muted-foreground">
                        Added {new Date(passkey.created_at).toLocaleDateString()}
                        {passkey.last_used_at
                          ? ` · Last used ${new Date(passkey.last_used_at).toLocaleString()}`
                          : " · Never used"}
                      </div>
                    </div>
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => deletePasskey(passkey.id)}
                    >
                      Remove
                    </Button>
                  </li>
                ))}
              </ul>
            )}
            <div className="flex gap-2">
              <Input
                placeholder="Name, e.g. MacBook Touch ID"
                value={passkeyName}
                onChange={(e) => setPasskeyName(e.target.value)}
              />
              <Button
                onClick={addPasskey}
                disabled={passkeyBusy || !passkeyName.trim()}
              >
                {passkeyBusy ? "Waiting…" : "Add passkey"}
              </Button>
            </div>
          </CardContent>
        </Card>
      )}

      <Dialog open={disableOpen} onOpenChange={setDisableOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Disable two-factor authentication</DialogTitle>
            <DialogDescription>Confirm with your password.</DialogDescription>
          </DialogHeader>
          <div className="grid gap-2">
            <Label>Password</Label>
            <Input
              type="password"
              value={disablePassword}
              onChange={(e) => setDisablePassword(e.target.value)}
            />
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDisableOpen(false)}>
              Cancel
            </Button>
            <Button
              variant="destructive"
              onClick={async () => {
                try {
                  await authApi.totpDisable(disablePassword)
                  setDisableOpen(false)
                  setDisablePassword("")
                  await refresh()
                  toast.success("Two-factor authentication is off")
                } catch (err) {
                  errorToast(err, "Could not disable 2FA")
                }
              }}
            >
              Disable
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}
