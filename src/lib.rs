//! Ferrocord: a lightweight, unofficial native Discord client.
//!
//! The crate is split into a library (so integration tests can reach every
//! module) and a thin binary in `main.rs`.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod app;
pub mod auth;
pub mod cli;
pub mod config;
pub mod error;
pub mod media;
pub mod mock;
pub mod net;
pub mod store;
pub mod tray;
pub mod ui;
#[cfg(windows)]
pub mod win_app_id;
