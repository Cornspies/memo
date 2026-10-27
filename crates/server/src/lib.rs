//! memo-server: authentication, access control, ordering and storage for memo clients. It never
//! decrypts anything. See `docs/server.md`.

pub mod account;
pub mod admin;
pub mod app;
pub mod attachments;
pub mod auth;
pub mod config;
pub mod db;
pub mod error;
pub mod jobs;
pub mod memos;
pub mod ratelimit;
pub mod state;
pub mod sync;
pub mod web;

pub use app::{router, serve};
pub use config::Config;
pub use state::{App, AppState};
