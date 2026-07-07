//! The [`DeepResearch`] async trait.
//!
//! Deep-research backends (GPT Researcher, `open_deep_research`, You.com
//! `DeepResearch`, Valyu) do not fit the single-round-trip [`super::Provider`]
//! shape — they run for minutes to hours. This trait models them as a
//! three-step lifecycle:
//!
//! 1. [`DeepResearch::submit`] — hand the orchestrator a query, get back a
//!    [`super::TaskId`].
//! 2. [`DeepResearch::poll`] — caller polls for status until
//!    [`super::ResearchStatus::is_ready`] returns `true`.
//! 3. [`DeepResearch::fetch`] — retrieve the final
//!    [`crate::ResearchResult`].

use crate::ResearchResult;
use crate::constraints::{DeepDepth, ResearchStatus, TaskId};
use crate::error::Result;
use crate::provider::BoxFut;

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
/// - `fetch` on an in-flight (pending / running) task must fail with the
///   transient-classified [`crate::Error::TaskNotReady`] so retry logic
///   keeps polling — never block indefinitely waiting for completion.
/// - `fetch` on a failed, cancelled, or unknown task must fail with the
///   permanent-classified [`crate::Error::TaskUnavailable`] so callers
///   stop retrying and resubmit instead.
///
/// # Cancellation
///
/// `submit` and `poll` are cancel-safe. `fetch` is cancel-safe as long as
/// the backend does not delete the task on retrieval (most don't; a few
/// do — implementations that do must document this in their own rustdoc).
pub trait DeepResearch: Send + Sync {
    /// Stable deep-research backend identifier.
    fn name(&self) -> &'static str;

    /// Submit a query for deep research. Returns the task identifier the
    /// caller will poll.
    ///
    /// # Errors
    ///
    /// The returned future resolves to [`crate::Error`] if submission
    /// fails at the transport layer or if the backend rejects the query.
    fn submit<'a>(&'a self, query: &'a str, depth: DeepDepth) -> BoxFut<'a, Result<TaskId>>;

    /// Check the lifecycle status of a previously-submitted task.
    ///
    /// # Errors
    ///
    /// The returned future resolves to [`crate::Error`] if the backend
    /// cannot be reached or if the task is unknown.
    fn poll<'a>(&'a self, task: &'a TaskId) -> BoxFut<'a, Result<ResearchStatus>>;

    /// Fetch the final result for a task. Only safe to call after
    /// [`DeepResearch::poll`] returns a status where
    /// [`ResearchStatus::is_ready`] is `true`.
    ///
    /// # Errors
    ///
    /// The returned future resolves to [`crate::Error::TaskNotReady`]
    /// (transient) while the task is still pending or running, and to
    /// [`crate::Error::TaskUnavailable`] (permanent) for failed,
    /// cancelled, or unknown tasks.
    fn fetch<'a>(&'a self, task: &'a TaskId) -> BoxFut<'a, Result<ResearchResult>>;
}
