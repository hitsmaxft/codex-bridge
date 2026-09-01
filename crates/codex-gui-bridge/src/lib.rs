//! codex-gui-bridge: WebSocket broker + app-server supervisor that lets a CLI
//! operate the same Codex Desktop sessions the GUI is using.

pub mod broker;
pub mod cli_api;
pub mod protocol;
pub mod supervisor;
