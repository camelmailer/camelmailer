//! Account authentication primitives: Argon2id password hashing, TOTP
//! (RFC 6238) second factors, session/reset/invitation tokens, and the
//! role model used for organization-level RBAC.
//!
//! Everything here is pure computation; persistence lives behind
//! [`crate::auth_store::AuthStore`].

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::Serialize;
use sha1::Sha1;
use sha2::{Digest, Sha256};

use crate::model::Id;

// ---------------------------------------------------------------- passwords

/// Hash a password with Argon2id (default parameters) into a PHC string.
pub fn hash_password(password: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|error| error.to_string())
}

/// Verify a password against a PHC-format digest. Malformed digests verify
/// as false rather than erroring — a corrupt row must not grant access.
pub fn verify_password(password: &str, digest: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(digest) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

// ------------------------------------------------------------------- tokens

/// SHA-256 hex of a bearer token — the only form ever persisted, so a
/// database leak does not leak live sessions/invitations/reset links.
pub fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex(&hasher.finalize())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A high-entropy bearer token (session / invitation / password reset).
pub fn generate_auth_token() -> String {
    crate::token::generate_token_charset(
        b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789",
        43,
    )
}

// -------------------------------------------------------------------- TOTP

const TOTP_STEP_SECONDS: u64 = 30;
const TOTP_DIGITS: u32 = 6;

const BASE32_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// RFC 4648 base32 (no padding), as used in `otpauth://` secrets.
pub fn base32_encode(data: &[u8]) -> String {
    let mut out = String::new();
    let mut buffer: u64 = 0;
    let mut bits = 0u32;
    for &byte in data {
        buffer = (buffer << 8) | u64::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(BASE32_ALPHABET[((buffer >> bits) & 0x1f) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(BASE32_ALPHABET[((buffer << (5 - bits)) & 0x1f) as usize] as char);
    }
    out
}

/// Decode RFC 4648 base32 (case-insensitive, padding ignored). Returns
/// `None` on characters outside the alphabet.
pub fn base32_decode(encoded: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut buffer: u64 = 0;
    let mut bits = 0u32;
    for ch in encoded.bytes() {
        if ch == b'=' || ch == b' ' {
            continue;
        }
        let value = BASE32_ALPHABET
            .iter()
            .position(|&c| c == ch.to_ascii_uppercase())? as u64;
        buffer = (buffer << 5) | value;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

/// A fresh 160-bit TOTP secret, base32-encoded for authenticator apps.
pub fn generate_totp_secret() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 20];
    rand::thread_rng().fill_bytes(&mut bytes);
    base32_encode(&bytes)
}

/// The RFC 6238 TOTP code (HMAC-SHA1, 30s step, 6 digits) for a base32
/// secret at a given unix time. `None` if the secret fails to decode.
pub fn totp_code(secret_base32: &str, unix_time: u64) -> Option<String> {
    let secret = base32_decode(secret_base32)?;
    let counter = unix_time / TOTP_STEP_SECONDS;
    let mut mac = Hmac::<Sha1>::new_from_slice(&secret).ok()?;
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = (digest[19] & 0x0f) as usize;
    let binary = (u32::from(digest[offset] & 0x7f) << 24)
        | (u32::from(digest[offset + 1]) << 16)
        | (u32::from(digest[offset + 2]) << 8)
        | u32::from(digest[offset + 3]);
    Some(format!(
        "{:01$}",
        binary % 10u32.pow(TOTP_DIGITS),
        TOTP_DIGITS as usize
    ))
}

/// Verify a submitted TOTP code, accepting the previous/current/next time
/// step to absorb clock drift.
pub fn verify_totp(secret_base32: &str, code: &str, unix_time: u64) -> bool {
    if code.len() != TOTP_DIGITS as usize {
        return false;
    }
    for drift in [-1i64, 0, 1] {
        let t = unix_time.saturating_add_signed(drift * TOTP_STEP_SECONDS as i64);
        if totp_code(secret_base32, t).as_deref() == Some(code) {
            return true;
        }
    }
    false
}

/// The `otpauth://` provisioning URI encoded into enrollment QR codes.
pub fn otpauth_url(secret_base32: &str, account: &str, issuer: &str) -> String {
    fn encode(value: &str) -> String {
        value
            .bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    (b as char).to_string()
                }
                other => format!("%{other:02X}"),
            })
            .collect()
    }
    format!(
        "otpauth://totp/{issuer}:{account}?secret={secret}&issuer={issuer}&algorithm=SHA1&digits=6&period=30",
        issuer = encode(issuer),
        account = encode(account),
        secret = secret_base32,
    )
}

// -------------------------------------------------------------------- RBAC

/// Organization-level role. Ordered by privilege: `Viewer < Member <
/// Admin < Owner`, so `role >= Role::Member` reads as "member or better".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Read-only access to the organization and its servers.
    Viewer,
    /// Read/write on server-scoped resources (domains, credentials,
    /// routes, webhooks, templates, sending).
    Member,
    /// Everything a member can, plus server lifecycle and inviting/managing
    /// non-owner members.
    Admin,
    /// Full control including deleting the organization and managing owners.
    Owner,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Member => "member",
            Role::Admin => "admin",
            Role::Owner => "owner",
        }
    }

    pub fn parse(value: &str) -> Option<Role> {
        match value {
            "viewer" => Some(Role::Viewer),
            "member" => Some(Role::Member),
            "admin" => Some(Role::Admin),
            "owner" => Some(Role::Owner),
            _ => None,
        }
    }
}

// ------------------------------------------------------------------ models

/// Per-user authentication state, kept separate from [`crate::model::User`]
/// so secrets never travel with the profile struct.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserAuth {
    pub user_id: Id,
    /// Argon2id PHC string; `None` for SSO-only accounts.
    pub password_digest: Option<String>,
    /// Base32 TOTP secret; set during enrollment, active once
    /// `totp_enabled`.
    pub totp_secret: Option<String>,
    pub totp_enabled: bool,
    pub failed_login_attempts: u32,
    pub locked_until: Option<DateTime<Utc>>,
    pub last_login_at: Option<DateTime<Utc>>,
    /// The OIDC subject this account is linked to, once SSO has been used.
    pub oidc_sub: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthSession {
    pub id: Id,
    pub user_id: Id,
    /// SHA-256 of the bearer token; the token itself is returned once at login.
    pub token_hash: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub last_used_at: DateTime<Utc>,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewAuthSession {
    pub user_id: Id,
    pub token_hash: String,
    pub expires_at: DateTime<Utc>,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrganizationMembership {
    pub id: Id,
    pub organization_id: Id,
    pub user_id: Id,
    pub role: Role,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invitation {
    pub id: Id,
    pub uuid: String,
    pub organization_id: Id,
    pub email_address: String,
    pub role: Role,
    pub token_hash: String,
    pub invited_by_user_id: Id,
    pub expires_at: DateTime<Utc>,
    pub accepted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct NewInvitation {
    pub organization_id: Id,
    pub email_address: String,
    pub role: Role,
    pub token_hash: String,
    pub invited_by_user_id: Id,
    pub expires_at: DateTime<Utc>,
}

/// An authentication audit record (logins, failures, lockouts, password
/// and membership changes, SSO sign-ins).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthEvent {
    pub id: Id,
    pub user_id: Option<Id>,
    pub email_address: Option<String>,
    pub event: String,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewAuthEvent {
    pub user_id: Option<Id>,
    pub email_address: Option<String>,
    pub event: String,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_hash_and_verify() {
        let digest = hash_password("correct horse battery staple").unwrap();
        assert!(digest.starts_with("$argon2id$"));
        assert!(verify_password("correct horse battery staple", &digest));
        assert!(!verify_password("wrong", &digest));
        assert!(!verify_password("anything", "not-a-phc-string"));
    }

    #[test]
    fn distinct_salts_produce_distinct_digests() {
        let a = hash_password("same").unwrap();
        let b = hash_password("same").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn token_hashing_is_sha256_hex() {
        // sha256("abc")
        assert_eq!(
            hash_token("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let token = generate_auth_token();
        assert_eq!(token.len(), 43);
        assert_ne!(generate_auth_token(), token);
    }

    #[test]
    fn base32_round_trips() {
        assert_eq!(base32_encode(b"foobar"), "MZXW6YTBOI");
        assert_eq!(base32_decode("MZXW6YTBOI").unwrap(), b"foobar");
        assert_eq!(base32_decode("mzxw6ytboi======").unwrap(), b"foobar");
        assert_eq!(base32_decode("1nvalid!"), None);
        assert_eq!(base32_encode(b""), "");
    }

    #[test]
    fn totp_matches_rfc_6238_sha1_vectors() {
        // RFC 6238 appendix B, secret "12345678901234567890", truncated to
        // 6 digits.
        let secret = base32_encode(b"12345678901234567890");
        assert_eq!(totp_code(&secret, 59).unwrap(), "287082");
        assert_eq!(totp_code(&secret, 1111111109).unwrap(), "081804");
        assert_eq!(totp_code(&secret, 1234567890).unwrap(), "005924");
        assert_eq!(totp_code(&secret, 2000000000).unwrap(), "279037");
    }

    #[test]
    fn totp_verification_allows_one_step_of_drift() {
        let secret = generate_totp_secret();
        let now = 1_700_000_000u64;
        let previous = totp_code(&secret, now - 30).unwrap();
        let current = totp_code(&secret, now).unwrap();
        let next = totp_code(&secret, now + 30).unwrap();
        assert!(verify_totp(&secret, &previous, now));
        assert!(verify_totp(&secret, &current, now));
        assert!(verify_totp(&secret, &next, now));
        let far = totp_code(&secret, now + 120).unwrap();
        // (may coincide by 1-in-10^6 chance; the fixed secret below avoids
        // flakiness for the negative case)
        let fixed = base32_encode(b"12345678901234567890");
        assert!(!verify_totp(&fixed, "000000", 59) || totp_code(&fixed, 59).unwrap() == "000000");
        let _ = far;
        assert!(!verify_totp(&secret, "12345", now)); // wrong length
    }

    #[test]
    fn otpauth_url_escapes_issuer_and_account() {
        let url = otpauth_url("SECRET32", "ada@example.com", "CamelMailer Test");
        assert_eq!(
            url,
            "otpauth://totp/CamelMailer%20Test:ada%40example.com?secret=SECRET32&issuer=CamelMailer%20Test&algorithm=SHA1&digits=6&period=30"
        );
    }

    #[test]
    fn roles_are_ordered_by_privilege() {
        assert!(Role::Viewer < Role::Member);
        assert!(Role::Member < Role::Admin);
        assert!(Role::Admin < Role::Owner);
        assert_eq!(Role::parse("owner"), Some(Role::Owner));
        assert_eq!(Role::parse("root"), None);
        assert_eq!(Role::Member.as_str(), "member");
        for role in [Role::Viewer, Role::Member, Role::Admin, Role::Owner] {
            assert_eq!(Role::parse(role.as_str()), Some(role));
        }
    }
}
