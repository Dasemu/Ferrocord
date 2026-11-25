//! SDK for Ferrocord plugins.
//!
//! A plugin is a WebAssembly module (`wasm32-unknown-unknown`) plus a
//! `plugin.toml` manifest. With this crate a plugin is a few lines:
//!
//! ```ignore
//! use ferrocord_plugin_sdk::prelude::*;
//!
//! #[derive(Default)]
//! struct Shout;
//!
//! impl Plugin for Shout {
//!     fn transform(&mut self, input: TransformInput) -> Option<String> {
//!         input.text.strip_prefix("!shout ").map(str::to_uppercase)
//!     }
//! }
//!
//! register_plugin!(Shout);
//! ```
//!
//! The [`abi`] module is the language-neutral contract (also documented in
//! `docs/plugins.md`); any language that compiles to core WebAssembly can
//! implement it directly.

pub mod abi;

/// Re-exported for `Vec<u8>` fields encoded as MessagePack `bin`.
pub use serde_bytes;

#[cfg(feature = "guest")]
pub mod guest;
#[cfg(feature = "guest")]
pub mod host;

#[cfg(feature = "guest")]
pub mod prelude {
    pub use crate::abi::{
        ActionInput, CommandInput, Decoration, DecorationKind, Event, HttpRequest, HttpResponse,
        LoadInfo, MessageInfo, SettingValue, TransformInput,
    };
    pub use crate::guest::Plugin;
    pub use crate::host::{self, HostError};
    pub use crate::register_plugin;
}
