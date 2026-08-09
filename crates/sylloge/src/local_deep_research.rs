//! Local deep-research backend lifecycle scaffold.
//!
//! The eventual Phase 05 backend will run the local-deep-researcher loop
//! (`generate_query -> web_research -> summarize_sources ->
//! reflect_on_summary -> finalize_summary`) against local model/search
//! adapters. This type owns the in-memory task lifecycle required by
//! [`crate::DeepResearch`] and can execute a deterministic offline loop
//! fixture without changing the public submit/poll/fetch contract.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

use jiff::Timestamp;

use crate::ResearchResult;
use crate::constraints::{DeepDepth, ResearchStatus, TaskId};
use crate::deep::DeepResearch;
use crate::error::{
    FatalCorruptionSnafu, InvalidQuerySnafu, RateLimitedSnafu, Result, TaskNotReadySnafu,
    TaskUnavailableSnafu,
};
use crate::fixture::{OfflineFixture, QueryGenerator, SourceRetriever, Synthesizer};
use crate::provider::BoxFut;
use crate::query::QueryShape;

/// In-memory lifecycle scaffold for the future local deep-research backend.
///
/// `LocalDeepResearch` does not yet call a local LLM or search provider. It
/// provides the task storage semantics the backend needs: submit creates a
/// task, poll observes lifecycle state, and fetch only succeeds after a
/// fixture or future worker marks that task ready with a [`ResearchResult`].
///
/// # Growth policy
///
/// The task store holds at most [`LocalDeepResearch::MAX_TASKS`] entries.
/// When `submit` finds the store full it first evicts terminal (ready /
/// failed / cancelled) tasks; if every task is still in flight it rejects
/// the submission with the transient [`crate::Error::RateLimited`] so
/// callers back off and retry. [`LocalDeepResearch::evict_terminal`] is the
/// explicit-cleanup form.
#[derive(Debug, Default)]
pub struct LocalDeepResearch {
    tasks: Mutex<HashMap<TaskId, TaskRecord>>,
    next_task: AtomicU64,
}

impl LocalDeepResearch {
    /// Maximum number of tasks the in-memory store retains, terminal or
    /// otherwise. See the type-level growth policy.
    pub const MAX_TASKS: usize = 1024;

    /// Construct an empty local deep-research backend.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark a submitted task as running.
    ///
    /// The progress value is clamped by [`ResearchStatus::running`].
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::TaskUnavailable`] if the task is unknown.
    pub fn mark_running(&self, task: &TaskId, progress_pct: Option<u8>) -> Result<()> {
        let mut tasks = self.lock_tasks()?;
        let record = task_record_mut(&mut tasks, task)?;
        record.status = ResearchStatus::running(progress_pct);
        Ok(())
    }

    /// Complete a submitted task with its final normalized result.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::TaskUnavailable`] if the task is unknown.
    pub fn complete_task(
        &self,
        task: &TaskId,
        result: ResearchResult,
        completed_at: Timestamp,
    ) -> Result<()> {
        let mut tasks = self.lock_tasks()?;
        let record = task_record_mut(&mut tasks, task)?;
        record.status = ResearchStatus::Ready { completed_at };
        record.result = Some(result);
        Ok(())
    }

    /// Mark a submitted task as failed.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::TaskUnavailable`] if the task is unknown.
    pub fn fail_task(&self, task: &TaskId, message: impl Into<String>) -> Result<()> {
        let mut tasks = self.lock_tasks()?;
        let record = task_record_mut(&mut tasks, task)?;
        record.status = ResearchStatus::Failed {
            message: message.into(),
        };
        record.result = None;
        Ok(())
    }

    /// Cancel a submitted task. Idempotent for pending, running, and
    /// already-cancelled tasks. Rejects a task that already reached
    /// [`ResearchStatus::Ready`] or [`ResearchStatus::Failed`] instead of
    /// silently discarding its result — see [`DeepResearch::cancel`] for
    /// the full per-state contract this implements.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::TaskUnavailable`] if the task is unknown,
    /// or if it already reached [`ResearchStatus::Ready`] or
    /// [`ResearchStatus::Failed`].
    pub fn cancel_task(&self, task: &TaskId) -> Result<()> {
        let mut tasks = self.lock_tasks()?;
        let record = task_record_mut(&mut tasks, task)?;
        match record.status {
            ResearchStatus::Pending | ResearchStatus::Running { .. } => {
                record.status = ResearchStatus::Cancelled;
                record.result = None;
                Ok(())
            }
            ResearchStatus::Cancelled => Ok(()),
            ResearchStatus::Ready { .. } => Err(TaskUnavailableSnafu {
                task: task.to_string(),
                reason: "task already completed; a finished result cannot be retroactively \
                         cancelled"
                    .to_owned(),
            }
            .build()),
            ResearchStatus::Failed { .. } => Err(TaskUnavailableSnafu {
                task: task.to_string(),
                reason: "task already failed; there is nothing left to cancel".to_owned(),
            }
            .build()),
        }
    }

    /// Number of tasks currently held by this in-memory backend.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::FatalCorruption`] if the task store mutex is
    /// poisoned.
    pub fn task_count(&self) -> Result<usize> {
        Ok(self.lock_tasks()?.len())
    }

    /// Remove every terminal (ready / failed / cancelled) task, returning
    /// how many were evicted. `submit` performs the same eviction
    /// automatically when the store reaches [`LocalDeepResearch::MAX_TASKS`].
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::FatalCorruption`] if the task store mutex is
    /// poisoned.
    pub fn evict_terminal(&self) -> Result<usize> {
        let mut tasks = self.lock_tasks()?;
        let before = tasks.len();
        tasks.retain(|_, record| !record.status.is_terminal());
        Ok(before - tasks.len())
    }

    /// Execute a previously-submitted task using an offline fixture.
    ///
    /// Atomically claims the pending task (marking it running), drives the
    /// five-node loop, then commits the result only if the task is still
    /// running — a concurrent `cancel_task` / `fail_task` between claim and
    /// commit wins, and the stale fixture result is discarded instead of
    /// resurrecting a terminal task.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::TaskUnavailable`] if the task is unknown,
    /// not in the pending state at claim time, or transitioned to another
    /// state mid-execution; [`crate::Error::FatalCorruption`] if the task
    /// store mutex is poisoned.
    pub fn execute_offline<Q, R, S>(
        &self,
        task: &TaskId,
        fixture: &OfflineFixture<Q, R, S>,
        shape: QueryShape,
    ) -> Result<ResearchResult>
    where
        Q: QueryGenerator,
        R: SourceRetriever,
        S: Synthesizer,
    {
        // Claim: verify Pending and mark Running under one lock so two
        // executors cannot both claim the task.
        let (query, depth) = {
            let mut tasks = self.lock_tasks()?;
            let record = task_record_mut(&mut tasks, task)?;
            if !matches!(record.status, ResearchStatus::Pending) {
                return Err(TaskUnavailableSnafu {
                    task: task.to_string(),
                    reason: format!(
                        "cannot execute task in state '{}'; only pending tasks are executable",
                        record.status.state_name()
                    ),
                }
                .build());
            }
            record.status = ResearchStatus::running(Some(0));
            (record.query.clone(), record.depth)
        };

        // The fixture runs outside the lock: it is pure compute over the
        // cloned query and must not block other lifecycle calls.
        let result = fixture.run(&query, shape, depth);

        // Commit: only a still-running task accepts the result.
        let mut tasks = self.lock_tasks()?;
        let record = task_record_mut(&mut tasks, task)?;
        if !matches!(record.status, ResearchStatus::Running { .. }) {
            return Err(TaskUnavailableSnafu {
                task: task.to_string(),
                reason: format!(
                    "task transitioned to '{}' during offline execution; result discarded",
                    record.status.state_name()
                ),
            }
            .build());
        }
        record.status = ResearchStatus::Ready {
            completed_at: Timestamp::now(),
        };
        record.result = Some(result.clone());
        Ok(result)
    }

    fn lock_tasks(&self) -> Result<MutexGuard<'_, HashMap<TaskId, TaskRecord>>> {
        self.tasks.lock().map_err(|_| {
            FatalCorruptionSnafu {
                message: "local deep-research task store mutex poisoned".to_owned(),
            }
            .build()
        })
    }
}

impl DeepResearch for LocalDeepResearch {
    fn name(&self) -> &'static str {
        "local_deep_research"
    }

    fn submit<'a>(&'a self, query: &'a str, depth: DeepDepth) -> BoxFut<'a, Result<TaskId>> {
        Box::pin(async move {
            if query.trim().is_empty() {
                return Err(InvalidQuerySnafu {
                    reason: "deep-research query must not be empty".to_owned(),
                }
                .build());
            }

            let mut tasks = self.lock_tasks()?;
            if tasks.len() >= Self::MAX_TASKS {
                tasks.retain(|_, record| !record.status.is_terminal());
            }
            if tasks.len() >= Self::MAX_TASKS {
                // Every retained task is still in flight; apply
                // backpressure rather than growing without bound.
                return Err(RateLimitedSnafu {
                    provider: "local_deep_research".to_owned(),
                    retry_after_ms: None::<u64>,
                }
                .build());
            }

            let sequence = self.next_task.fetch_add(1, Ordering::Relaxed) + 1;
            let task = TaskId::new(format!("local-deep-research-{sequence}"));
            let record = TaskRecord {
                status: ResearchStatus::Pending,
                result: None,
                query: query.to_owned(),
                depth,
            };
            tasks.insert(task.clone(), record);
            Ok(task)
        })
    }

    fn poll<'a>(&'a self, task: &'a TaskId) -> BoxFut<'a, Result<ResearchStatus>> {
        Box::pin(async move {
            let tasks = self.lock_tasks()?;
            let record = task_record(&tasks, task)?;
            Ok(record.status.clone())
        })
    }

    fn fetch<'a>(&'a self, task: &'a TaskId) -> BoxFut<'a, Result<ResearchResult>> {
        Box::pin(async move {
            let tasks = self.lock_tasks()?;
            let record = task_record(&tasks, task)?;
            match &record.status {
                ResearchStatus::Ready { .. } => record.result.clone().ok_or_else(|| {
                    FatalCorruptionSnafu {
                        message: format!("ready task '{task}' has no local deep-research result"),
                    }
                    .build()
                }),
                ResearchStatus::Pending | ResearchStatus::Running { .. } => {
                    Err(TaskNotReadySnafu {
                        task: task.to_string(),
                        detail: format!(
                            "state={}; query='{}', depth={}; task is not ready — poll and retry",
                            record.status.state_name(),
                            record.query,
                            record.depth.as_str()
                        ),
                    }
                    .build())
                }
                ResearchStatus::Failed { message } => Err(TaskUnavailableSnafu {
                    task: task.to_string(),
                    reason: format!("task failed: {message}"),
                }
                .build()),
                ResearchStatus::Cancelled => Err(TaskUnavailableSnafu {
                    task: task.to_string(),
                    reason: "task was cancelled".to_owned(),
                }
                .build()),
            }
        })
    }

    fn cancel<'a>(&'a self, task: &'a TaskId) -> BoxFut<'a, Result<()>> {
        Box::pin(async move { self.cancel_task(task) })
    }
}

#[derive(Debug, Clone)]
struct TaskRecord {
    status: ResearchStatus,
    result: Option<ResearchResult>,
    query: String,
    depth: DeepDepth,
}

fn task_record<'a>(
    tasks: &'a HashMap<TaskId, TaskRecord>,
    task: &TaskId,
) -> Result<&'a TaskRecord> {
    tasks.get(task).ok_or_else(|| unknown_task(task))
}

fn task_record_mut<'a>(
    tasks: &'a mut HashMap<TaskId, TaskRecord>,
    task: &TaskId,
) -> Result<&'a mut TaskRecord> {
    tasks.get_mut(task).ok_or_else(|| unknown_task(task))
}

fn unknown_task(task: &TaskId) -> crate::Error {
    TaskUnavailableSnafu {
        task: task.to_string(),
        reason: "unknown task".to_owned(),
    }
    .build()
}
