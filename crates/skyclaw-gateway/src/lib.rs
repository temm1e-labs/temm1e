//! SkyClaw Gateway crate — HTTP/WebSocket gateway that routes messages
//! between messaging channels and the agent runtime.

pub mod health;
pub mod router;
pub mod server;
pub mod session;

pub use server::SkyGate;
