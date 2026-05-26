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

use async_trait::async_trait;
use jiff::Timestamp;

use crate::ResearchResult;
use crate::constraints::{DeepDepth, ResearchStatus, TaskId};
use crate::deep::DeepResearch;
use crate::error::{FatalCorruptionSnafu, InvalidQuerySnafu, Result, UnsupportedSnafu};
use crate::fixture::{OfflineFixture, QueryGenerator, SourceRetriever, Synthesizer};
use crate::query::QueryShape;

/// In-memory lifecycle scaffold for the future local deep-research backend.
///
/// `LocalDeepResearch` does not yet call a local LLM or search provider. It
/// provides the task storage semantics the backend needs: submit creates a
/// task, poll observes lifecycle state, and fetch only succeeds after a
/// fixture or future worker marks that task ready with a [`ResearchResult`].
#[derive(Debug, Default)]
pub struct LocalDeepResearch {
    tasks: Mutex<HashMap<TaskId, TaskRecord>>,
    next_task: AtomicU64,
}

impl LocalDeepResearch {
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
    /// Returns [`crate::Error::Unsupported`] if the task is unknown.
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
    /// Returns [`crate::Error::Unsupported`] if the task is unknown.
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
    /// Returns [`crate::Error::Unsupported`] if the task is unknown.
    pub fn fail_task(&self, task: &TaskId, message: impl Into<String>) -> Result<()> {
        let mut tasks = self.lock_tasks()?;
        let record = task_record_mut(&mut tasks, task)?;
        record.status = ResearchStatus::Failed {
            message: message.into(),
        };
        record.result = None;
        Ok(())
    }

    /// Mark a submitted task as cancelled.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Unsupported`] if the task is unknown.
    pub fn cancel_task(&self, task: &TaskId) -> Result<()> {
        let mut tasks = self.lock_tasks()?;
        let record = task_record_mut(&mut tasks, task)?;
        record.status = ResearchStatus::Cancelled;
        record.result = None;
        Ok(())
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

    /// Execute a previously-submitted task using an offline fixture.
    ///
    /// Marks the task running, drives the five-node loop, and completes the
    /// task with the resulting normalized [`ResearchResult`].
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Unsupported`] if the task is unknown, or
    /// [`crate::Error::FatalCorruption`] if the task store mutex is poisoned.
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
        let (query, depth) = {
            let tasks = self.lock_tasks()?;
            let record = task_record(&tasks, task)?;
            (record.query.clone(), record.depth)
        };

        self.mark_running(task, Some(0))?;
        let result = fixture.run(&query, shape, depth);
        self.complete_task(task, result.clone(), Timestamp::now())?;
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

#[async_trait]
impl DeepResearch for LocalDeepResearch {
    fn name(&self) -> &'static str {
        "local_deep_research"
    }

    async fn submit(&self, query: &str, depth: DeepDepth) -> Result<TaskId> {
        if query.trim().is_empty() {
            return Err(InvalidQuerySnafu {
                reason: "deep-research query must not be empty".to_owned(),
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
        self.lock_tasks()?.insert(task.clone(), record);
        Ok(task)
    }

    async fn poll(&self, task: &TaskId) -> Result<ResearchStatus> {
        let tasks = self.lock_tasks()?;
        let record = task_record(&tasks, task)?;
        Ok(record.status.clone())
    }

    async fn fetch(&self, task: &TaskId) -> Result<ResearchResult> {
        let tasks = self.lock_tasks()?;
        let record = task_record(&tasks, task)?;
        match &record.status {
            ResearchStatus::Ready { .. } => record.result.clone().ok_or_else(|| {
                FatalCorruptionSnafu {
                    message: format!("ready task '{task}' has no local deep-research result"),
                }
                .build()
            }),
            ResearchStatus::Pending | ResearchStatus::Running { .. } => Err(UnsupportedSnafu {
                reason: format!(
                    "local deep-research task '{task}' is not ready; query='{}', depth={}",
                    record.query,
                    record.depth.as_str()
                ),
            }
            .build()),
            ResearchStatus::Failed { message } => Err(UnsupportedSnafu {
                reason: format!("local deep-research task '{task}' failed: {message}"),
            }
            .build()),
            ResearchStatus::Cancelled => Err(UnsupportedSnafu {
                reason: format!("local deep-research task '{task}' was cancelled"),
            }
            .build()),
        }
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
    UnsupportedSnafu {
        reason: format!("unknown local deep-research task '{task}'"),
    }
    .build()
}
