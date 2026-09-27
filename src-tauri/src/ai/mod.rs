//! Built-in AI features backed by a model endpoint the user configures.
//!
//! [`config`] stores endpoints and their encrypted API keys in the policy file,
//! and [`provider`] talks to those endpoints.

pub mod config;
pub mod provider;
