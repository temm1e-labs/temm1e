//! SkyClaw Vault crate — encrypted secrets management, credential detection,
//! and `vault://` URI resolution.

pub mod local;
pub mod detector;
pub mod resolver;

pub use local::LocalVault;
pub use detector::{detect_credentials, DetectedCredential};
pub use resolver::{parse_vault_uri, is_vault_uri, resolve, VaultUri};
