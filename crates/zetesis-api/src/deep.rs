//! The [`DeepResearch`] async trait.
//!
//! Deep-research backends (GPT Researcher, open_deep_research, You.com
//! DeepResearch, Valyu) do not fit the single-round-trip [`super::Provider`]
//! shape — they run for minutes to hours. This trait models them as a
//! three-step lifecycle:
//!
//! 1. [`DeepResearch::submit`] — hand the orchestrator a query, get back a
//!    [`super::TaskId`].
//! 2. [`DeepResearch::poll`] — caller polls for status until
//!    [`super::ResearchStatus::is_ready`] returns `true`.
//! 3. [`DeepResearch::fetch`] — retrieve the final
//!    [`zetesis_types::ResearchResult`].

use async_trait::async_trait;

use zetesis_types::ResearchResult;

use crate::constraints::{DeepDepth, ResearchStatus, TaskId};
use crate::error::Result;

/// Multi-step deep-research backend.
///
/// Implementations must be `Send + Sync`.
///
/// # Contract
///
/// - `submit` must return promptly (upload the query; don't wait for
///   completion). Long-running work happens in the backend.
/// - `poll` must be idempotent. The router may poll aggressively to drive
///   UX progress indicators.
/// - `fetch` on a non-ready task must fail with
///   [`crate::Error::Unsupported`] or a provider-specific permanent
///   error — never block indefinitely waiting for completion.
///
/// # Cancellation
///
/// `submit` and `poll` are cancel-safe. `fetch` is cancel-safe as long as
/// the backend does not delete the task on retrieval (most don't; a few
/// do — implementations that do must document this in their own rustdoc).
#[async_trait]
pub trait DeepResearch: Send + Sync {
    /// Stable deep-research backend identifier.
    fn name(&self) -> &'static str;

    /// Submit a query for deep research. Returns the task identifier the
    /// caller will poll.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error`] if submission fails at the transport layer
    /// or if the backend rejects the query.
    async fn submit(&self, query: &str, depth: DeepDepth) -> Result<TaskId>;

    /// Check the lifecycle status of a previously-submitted task.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error`] if the backend cannot be reached or if the
    /// task is unknown.
    async fn poll(&self, task: &TaskId) -> Result<ResearchStatus>;

    /// Fetch the final result for a task. Only safe to call after
    /// [`DeepResearch::poll`] returns a status where
    /// [`ResearchStatus::is_ready`] is `true`.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error`] if the task is not yet ready, the result
    /// cannot be fetched, or the backend reports a permanent failure.
    async fn fetch(&self, task: &TaskId) -> Result<ResearchResult>;
}
