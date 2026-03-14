//! TEMM1E Vault crate — encrypted secrets management, credential detection,
//! and `vault://` URI resolution.

pub mod detector;
pub mod local;
pub mod resolver;

pub use detector::{detect_credentials, DetectedCredential};
pub use local::LocalVault;
pub use resolver::{is_vault_uri, parse_vault_uri, resolve, VaultUri};
