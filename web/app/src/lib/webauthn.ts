// WebAuthn browser glue: the backend (webauthn-rs) speaks JSON with every
// binary field as unpadded base64url; the browser credentials API wants
// ArrayBuffers. These helpers convert options in one direction and the
// authenticator responses in the other.

export function base64urlToBuffer(value: string): ArrayBuffer {
  const base64 = value.replace(/-/g, "+").replace(/_/g, "/")
  const padded = base64 + "=".repeat((4 - (base64.length % 4)) % 4)
  const raw = atob(padded)
  const bytes = new Uint8Array(raw.length)
  for (let i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i)
  return bytes.buffer
}

export function bufferToBase64url(buffer: ArrayBuffer): string {
  const bytes = new Uint8Array(buffer)
  let raw = ""
  for (const byte of bytes) raw += String.fromCharCode(byte)
  return btoa(raw).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "")
}

// The wire shapes of webauthn-rs' CreationChallengeResponse /
// RequestChallengeResponse (only the fields we must transform are typed;
// everything else passes through untouched).
type CredentialDescriptorJSON = {
  type: string
  id: string
  transports?: string[]
}

export type CreationOptionsJSON = {
  publicKey: {
    challenge: string
    user: { id: string; name: string; displayName: string }
    excludeCredentials?: CredentialDescriptorJSON[]
  } & Record<string, unknown>
}

export type RequestOptionsJSON = {
  publicKey: {
    challenge: string
    allowCredentials?: CredentialDescriptorJSON[]
  } & Record<string, unknown>
}

// What the backend expects at register/finish and login/finish — exactly
// the serde shape of webauthn-rs' RegisterPublicKeyCredential /
// PublicKeyCredential.
export type RegisterCredentialJSON = {
  id: string
  rawId: string
  type: string
  response: { attestationObject: string; clientDataJSON: string }
  clientExtensionResults: Record<string, unknown>
}

export type AssertionCredentialJSON = {
  id: string
  rawId: string
  type: string
  response: {
    authenticatorData: string
    clientDataJSON: string
    signature: string
    userHandle: string | null
  }
  clientExtensionResults: Record<string, unknown>
}

export function webAuthnSupported(): boolean {
  return typeof window !== "undefined" && !!window.PublicKeyCredential
}

function decodeDescriptors(
  descriptors: CredentialDescriptorJSON[] | undefined,
): PublicKeyCredentialDescriptor[] | undefined {
  return descriptors?.map((descriptor) => ({
    type: "public-key" as const,
    id: base64urlToBuffer(descriptor.id),
    transports: descriptor.transports as AuthenticatorTransport[] | undefined,
  }))
}

/** Run `navigator.credentials.create()` with the options JSON from
 * `POST /api/v2/auth/webauthn/register/start` and return the credential
 * in the JSON shape `register/finish` expects. */
export async function createPasskey(
  options: CreationOptionsJSON,
): Promise<RegisterCredentialJSON> {
  const publicKey = {
    ...options.publicKey,
    challenge: base64urlToBuffer(options.publicKey.challenge),
    user: {
      ...options.publicKey.user,
      id: base64urlToBuffer(options.publicKey.user.id),
    },
    excludeCredentials: decodeDescriptors(options.publicKey.excludeCredentials),
  } as unknown as PublicKeyCredentialCreationOptions
  const credential = (await navigator.credentials.create({
    publicKey,
  })) as PublicKeyCredential | null
  if (!credential) throw new Error("The passkey creation was cancelled")
  const response = credential.response as AuthenticatorAttestationResponse
  return {
    id: credential.id,
    rawId: bufferToBase64url(credential.rawId),
    type: credential.type,
    response: {
      attestationObject: bufferToBase64url(response.attestationObject),
      clientDataJSON: bufferToBase64url(response.clientDataJSON),
    },
    clientExtensionResults: credential.getClientExtensionResults() as Record<
      string,
      unknown
    >,
  }
}

/** Run `navigator.credentials.get()` with the options JSON from
 * `POST /api/v2/auth/webauthn/login/start` and return the assertion in
 * the JSON shape `login/finish` expects. */
export async function getPasskeyAssertion(
  options: RequestOptionsJSON,
): Promise<AssertionCredentialJSON> {
  const publicKey = {
    ...options.publicKey,
    challenge: base64urlToBuffer(options.publicKey.challenge),
    allowCredentials: decodeDescriptors(options.publicKey.allowCredentials),
  } as unknown as PublicKeyCredentialRequestOptions
  const credential = (await navigator.credentials.get({
    publicKey,
  })) as PublicKeyCredential | null
  if (!credential) throw new Error("The passkey sign-in was cancelled")
  const response = credential.response as AuthenticatorAssertionResponse
  return {
    id: credential.id,
    rawId: bufferToBase64url(credential.rawId),
    type: credential.type,
    response: {
      authenticatorData: bufferToBase64url(response.authenticatorData),
      clientDataJSON: bufferToBase64url(response.clientDataJSON),
      signature: bufferToBase64url(response.signature),
      userHandle: response.userHandle
        ? bufferToBase64url(response.userHandle)
        : null,
    },
    clientExtensionResults: credential.getClientExtensionResults() as Record<
      string,
      unknown
    >,
  }
}
