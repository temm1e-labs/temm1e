//! PKCE (Proof Key for Code Exchange) — S256 method
//!
//! Generates a verifier/challenge pair for the OAuth 2.0 PKCE extension.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use sha2::{Digest, Sha256};

/// A PKCE verifier + challenge pair.
pub struct PkceChallenge {
    /// The code_verifier — sent in the token exchange request.
    pub verifier: String,
    /// The code_challenge — sent in the authorization URL (S256 hash of verifier).
    pub challenge: String,
}

impl PkceChallenge {
    /// Generate a new PKCE challenge pair using S256 method.
    ///
    /// - verifier: 32 random bytes → base64url encoded (43 chars)
    /// - challenge: SHA-256(verifier) → base64url encoded
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        let verifier = URL_SAFE_NO_PAD.encode(bytes);

        let hash = Sha256::digest(verifier.as_bytes());
        let challenge = URL_SAFE_NO_PAD.encode(hash);

        Self {
            verifier,
            challenge,
        }
    }
}

/// Generate a random state string for OAuth CSRF protection.
pub fn generate_state() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_verifier_length() {
        let pkce = PkceChallenge::generate();
        assert_eq!(pkce.verifier.len(), 43);
    }

    #[test]
    fn pkce_challenge_differs_from_verifier() {
        let pkce = PkceChallenge::generate();
        assert_ne!(pkce.verifier, pkce.challenge);
    }

    #[test]
    fn pkce_challenge_is_sha256_of_verifier() {
        let pkce = PkceChallenge::generate();
        let hash = Sha256::digest(pkce.verifier.as_bytes());
        let expected = URL_SAFE_NO_PAD.encode(hash);
        assert_eq!(pkce.challenge, expected);
    }

    #[test]
    fn state_is_unique() {
        let s1 = generate_state();
        let s2 = generate_state();
        assert_ne!(s1, s2);
    }

    #[test]
    fn state_is_base64url() {
        let state = generate_state();
        assert!(URL_SAFE_NO_PAD.decode(&state).is_ok());
    }
}
