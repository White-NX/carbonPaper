//! Text-only storage and MCP dispatch. This crate has no archive/CNG/image API.
pub mod dispatcher;
mod embeddings;
#[path = "../../src/mcp_contract.rs"]
pub mod mcp_contract;
#[allow(dead_code)]
#[path = "../../src/ml_protocol.rs"]
mod ml_protocol;
#[path = "../../src/mcp_server/page.rs"]
mod page;
#[path = "../../src/pii/mod.rs"]
pub mod pii;
#[cfg(test)]
mod reader_tests;
mod recap;
mod recap_corrections;
#[path = "../../src/recap_text_contract.rs"]
mod recap_text_contract;
pub mod repository;
#[allow(dead_code)]
#[path = "../../src/storage/search_plan.rs"]
mod search_plan;
#[allow(dead_code)]
#[path = "../../src/storage/search_rank.rs"]
mod search_rank;
#[allow(dead_code)]
#[path = "../../src/semantic_models.rs"]
mod semantic_models;
#[path = "../../src/sensitive_filter.rs"]
pub mod sensitive_filter;
mod text_search;
pub mod transport;
pub mod types;
