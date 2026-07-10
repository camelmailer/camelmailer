//! Random token / identifier generation (ports the `Nifty::Utils::RandomString`
//! and `SecureRandom` usage scattered through the Ruby app).

use rand::Rng;

const ALPHANUMERIC: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
const LOWER_ALPHANUMERIC: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
const HEX: &[u8] = b"0123456789abcdef";

fn random_string(charset: &[u8], length: usize) -> String {
    let mut rng = rand::thread_rng();
    (0..length)
        .map(|_| charset[rng.gen_range(0..charset.len())] as char)
        .collect()
}

/// A server/route token (lowercase alphanumeric).
pub fn generate_token(length: usize) -> String {
    random_string(LOWER_ALPHANUMERIC, length)
}

/// An SMTP/API credential key.
pub fn generate_key() -> String {
    random_string(ALPHANUMERIC, 24)
}

/// A random string over a caller-supplied charset (used for high-entropy
/// bearer tokens).
pub fn generate_token_charset(charset: &[u8], length: usize) -> String {
    random_string(charset, length)
}

/// A per-connection trace id, e.g. `A1B2C3D4` (ports
/// `SecureRandom.alphanumeric(8).upcase`).
pub fn generate_trace_id() -> String {
    random_string(ALPHANUMERIC, 8).to_uppercase()
}

/// A random hex string (used for CRAM-MD5 challenges).
pub fn generate_hex(length: usize) -> String {
    random_string(HEX, length)
}

/// A UUID v4 string.
pub fn generate_uuid() -> String {
    let mut rng = rand::thread_rng();
    let mut bytes = [0u8; 16];
    rng.fill(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn tokens_have_requested_length_and_charset() {
        let token = generate_token(6);
        assert_eq!(token.len(), 6);
        assert!(token
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
    }

    #[test]
    fn trace_ids_are_uppercase_alphanumeric() {
        let trace_id = generate_trace_id();
        assert_eq!(trace_id.len(), 8);
        assert!(trace_id
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()));
    }

    #[test]
    fn uuids_are_v4_shaped() {
        let uuid = generate_uuid();
        assert_eq!(uuid.len(), 36);
        assert_eq!(uuid.chars().nth(14), Some('4'));
        let variant = uuid.chars().nth(19).unwrap();
        assert!(matches!(variant, '8' | '9' | 'a' | 'b'));
    }

    #[test]
    fn generated_values_are_unique_enough() {
        let mut seen = HashSet::new();
        for _ in 0..100 {
            assert!(seen.insert(generate_key()));
        }
    }
}
