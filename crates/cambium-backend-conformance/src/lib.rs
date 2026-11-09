//! Backend conformance suite and its fixtures (ADR-0007).
//!
//! [`suite::run`] checks a backend against the contracts in ADR-0007. Adapters
//! call it from an integration test against a real, pinned backend.

pub mod paths;
pub mod suite;
