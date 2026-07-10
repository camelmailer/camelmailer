//! RSA signing with the installation's signing key
//! (`camelmailer.signing_key_path`) — used for webhook payload signatures
//! and DKIM. The port of `lib/postal/signer.rb`.

use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::{Pkcs1v15Sign, RsaPrivateKey, RsaPublicKey};
use sha2::{Digest, Sha256};

#[derive(Clone)]
pub struct Signer {
    key: RsaPrivateKey,
}

impl Signer {
    pub fn from_pem(pem: &str) -> Result<Self, String> {
        let key = RsaPrivateKey::from_pkcs8_pem(pem)
            .or_else(|_| RsaPrivateKey::from_pkcs1_pem(pem))
            .map_err(|error| format!("could not parse RSA private key: {error}"))?;
        Ok(Self { key })
    }

    /// Load from the configured path; `None` when the file does not exist
    /// (signing is then disabled).
    pub fn from_pem_file(path: &str) -> std::io::Result<Option<Self>> {
        match std::fs::read_to_string(path) {
            Ok(pem) => Self::from_pem(&pem)
                .map(Some)
                .map_err(std::io::Error::other),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// PKCS#1 v1.5 RSA-SHA256 signature.
    pub fn sign_sha256(&self, data: &[u8]) -> Vec<u8> {
        let digest = Sha256::digest(data);
        self.key
            .sign(Pkcs1v15Sign::new::<Sha256>(), &digest)
            .expect("RSA signing cannot fail for a valid key")
    }

    pub fn public_key(&self) -> RsaPublicKey {
        self.key.to_public_key()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::signature::Verifier;

    #[test]
    fn signs_and_verifies_sha256() {
        let mut rng = rsa::rand_core::OsRng;
        let key = RsaPrivateKey::new(&mut rng, 1024).unwrap();
        let signer = Signer { key };
        let signature = signer.sign_sha256(b"payload");

        let verifying_key =
            rsa::pkcs1v15::VerifyingKey::<Sha256>::new(signer.public_key());
        let signature = rsa::pkcs1v15::Signature::try_from(signature.as_slice()).unwrap();
        verifying_key.verify(b"payload", &signature).unwrap();
    }
}
