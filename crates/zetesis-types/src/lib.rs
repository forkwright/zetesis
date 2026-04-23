//! Shared types for the zetesis sovereign research substrate.
//!
//! This crate holds the wire-neutral data structures that all other zetesis
//! crates (`zetesis-api`, `zetesis-providers`, `zetesis-router`, `zetesis-cache`,
//! `zetesis-budget`, `zetesis-orchestrator`) agree on. The types here do not
//! import any fleet-specific networking or storage crate so consumers can
//! depend on just this crate for interoperability.
//!
//! # Layout
//!
//! - [`QueryShape`] — the classifier category a query falls into (drives
//!   provider-tier routing decisions in `zetesis-router`).
//! - [`Citation`] — provenance record for a single retrieved document.
//! - [`ResultHit`] / [`ResearchResult`] — normalized per-hit and per-query
//!   result schemas.
//! - [`CostTracking`] — post-call spend ledger covering paid spend plus
//!   free-tier quota consumption.
//! - [`BudgetConstraint`] — pre-call ceiling enforced by `zetesis-budget`.
//! - [`ProviderTier`] — tier classification driving the free-first fallback
//!   chain.
//!
//! All public enums carry `#[non_exhaustive]` so new variants are not
//! breaking changes. All persisted types derive `serde` and are designed to
//! round-trip through JSON (wire) and CBOR (cache / ledger) without loss.

#![deny(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod budget;
mod citation;
mod cost;
mod query;
mod result;
mod tier;

pub use budget::BudgetConstraint;
pub use citation::{Citation, SourceKind};
pub use cost::{CostTracking, ProviderSpend};
pub use query::QueryShape;
pub use result::{ProvenanceEntry, ResearchResult, ResultHit};
pub use tier::ProviderTier;
