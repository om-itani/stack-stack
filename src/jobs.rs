//! Universal background job runner.
//!
//! Long-running operations (a `docker compose pull`, an image download, a backup)
//! need somewhere to report progress that outlives the request that started
//! them. This is that place: a process-wide registry of [`Job`]s plus the
//! [`JobCtx`] a worker uses to publish progress and log lines.
//!
//! Two entry points:
//!
//! * [`JobManager::track_with`] registers a job and runs an async future while
//!   the caller awaits the result. This is what the existing request/response
//!   handlers use: nothing about their contract changes, but the operation now
//!   shows up in the activity drawer and the completion toast.
//! * [`JobManager::spawn_job`] registers a job and detaches it, returning the id
//!   immediately. This is the hook future features (mergerfs formatting, VM
//!   cloning) call when there is no request to hold open.
//!
//! The registry lives in memory: a job is a description of work in flight, not
//! durable state, so surviving a restart is not a goal. Finished jobs are kept
//! for [`HISTORY_TTL_MS`] and then pruned. Every mutation takes one short
//! `RwLock`, so a job can never block the API.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::notify::{NotificationRequest, Notifier};

/// How long a finished job stays in the history the UI reads.
pub const HISTORY_TTL_MS: u64 = 24 * 60 * 60 * 1000;

/// Hard cap on retained jobs; the oldest finished ones are dropped first.
pub const MAX_JOBS: usize = 200;

/// A job that never finished (its worker wedged, or a cancelled request whose
/// guard did not run) is marked failed after this long, so it cannot sit in the
/// "running" count forever.
const STALE_JOB_MS: u64 = 6 * 60 * 60 * 1000;

/// How often the read path is allowed to take the write lock to prune.
const PRUNE_INTERVAL_MS: u64 = 60 * 1000;

/// How many log lines one job keeps (from the end).
const MAX_LOGS: usize = 200;

/// Stable event id used for the completion notifications.
pub const JOB_EVENT: &str = "job_finished";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Pending,
    Running,
    Success,
    Failed,
}

/// One tracked operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub name: String,
    pub status: JobStatus,
    /// Completion percentage, 0..=100.
    pub progress: u8,
    /// Tail of the job's own log lines.
    pub logs: Vec<String>,
    pub started_ms: u64,
    pub finished_ms: Option<u64>,
    /// Short summary set on success.
    pub message: Option<String>,
    /// Failure detail set on error.
    pub error: Option<String>,
    /// Whether completion is announced through the notification centre. True
    /// only for detached (`spawn_job`) work, so an interactive request that
    /// already reports its result does not also raise a notification.
    #[serde(skip)]
    pub announce: bool,
}

impl Job {
    pub fn is_finished(&self) -> bool {
        matches!(self.status, JobStatus::Success | JobStatus::Failed)
    }
}

#[derive(Default)]
struct Inner {
    jobs: HashMap<String, Job>,
    /// Insertion order, oldest first; the list endpoint walks it in reverse.
    order: VecDeque<String>,
    next: u64,
}

/// The process-wide registry. Cheap to clone (one `Arc`).
#[derive(Clone)]
pub struct JobManager {
    inner: Arc<RwLock<Inner>>,
    /// Optional completion sink; wired to the notification centre at startup.
    notifier: Option<Arc<dyn Notifier>>,
    /// Last time the registry was pruned, so the read path does not take the
    /// write lock on every poll.
    last_prune_ms: Arc<AtomicU64>,
}

impl JobManager {
    pub fn new(notifier: Option<Arc<dyn Notifier>>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Inner::default())),
            notifier,
            last_prune_ms: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Register an operation and run it inline, forwarding progress to the job.
    ///
    /// The operation's own result type and error type are preserved, so an HTTP
    /// handler keeps its existing contract; the job records the error text and,
    /// on success, the last line the worker logged (if any).
    pub async fn track_with<T, F, Fut, E>(
        &self,
        name: impl Into<String>,
        work: F,
    ) -> Result<T, E>
    where
        F: FnOnce(JobCtx) -> Fut,
        Fut: std::future::Future<Output = Result<T, E>>,
        E: std::fmt::Display,
    {
        let (id, ctx) = self.insert(name.into(), JobStatus::Running, false);
        // If the caller's future is dropped (the API deadline fires, or the
        // client disconnects), the guard still settles the job so it cannot sit
        // in "running" forever.
        let mut guard = SettleGuard {
            manager: self.clone(),
            id: id.clone(),
            settled: false,
        };
        match work(ctx).await {
            Ok(value) => {
                guard.settled = true;
                self.finish(&id, JobStatus::Success, None, None);
                Ok(value)
            }
            Err(err) => {
                guard.settled = true;
                self.finish(&id, JobStatus::Failed, None, Some(err.to_string()));
                Err(err)
            }
        }
    }

    /// Register an operation and detach it. Returns the job id immediately.
    ///
    /// The closure runs on the blocking pool so a slow shell-out never occupies
    /// an async worker. With no tokio runtime (unit tests) it runs inline.
    ///
    /// This is the hook for work that has no request to hold open (a future
    /// mergerfs format, a VM clone, a backup); the handlers that must return the
    /// operation's result use [`JobManager::track_with`] instead, so this is
    /// currently exercised only by its tests.
    #[allow(dead_code)]
    pub fn spawn_job<F>(&self, name: impl Into<String>, work: F) -> String
    where
        F: FnOnce(JobCtx) -> Result<String, String> + Send + 'static,
    {
        let (id, ctx) = self.insert(name.into(), JobStatus::Pending, true);
        let manager = self.clone();
        let id_for_task = id.clone();

        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    manager.mark_running(&id_for_task);
                    let result = tokio::task::spawn_blocking(move || work(ctx)).await;
                    match result {
                        Ok(Ok(message)) => {
                            manager.finish(&id_for_task, JobStatus::Success, Some(message), None)
                        }
                        Ok(Err(err)) => {
                            manager.finish(&id_for_task, JobStatus::Failed, None, Some(err))
                        }
                        Err(join) => manager.finish(
                            &id_for_task,
                            JobStatus::Failed,
                            None,
                            Some(format!("job task failed: {join}")),
                        ),
                    }
                });
            }
            Err(_) => {
                self.mark_running(&id);
                match work(ctx) {
                    Ok(message) => self.finish(&id, JobStatus::Success, Some(message), None),
                    Err(err) => self.finish(&id, JobStatus::Failed, None, Some(err)),
                }
            }
        }

        id
    }

    /// Newest-first snapshot for the drawer.
    ///
    /// Only the last log line is carried: the list is polled every few seconds
    /// and the drawer shows a single log line, so shipping the full tail would
    /// be work the client throws away.
    pub fn list(&self) -> Vec<Job> {
        self.maybe_prune();
        let inner = self.inner.read().expect("jobs lock");
        inner
            .order
            .iter()
            .rev()
            .filter_map(|id| inner.jobs.get(id).cloned())
            .map(|mut job| {
                if job.logs.len() > 1 {
                    let last = job.logs.split_off(job.logs.len() - 1);
                    job.logs = last;
                }
                job
            })
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<Job> {
        self.maybe_prune();
        self.inner
            .read()
            .expect("jobs lock")
            .jobs
            .get(id)
            .cloned()
    }

    /// Forget one job. Finished jobs can be cleared from history; a running job
    /// is left alone because its worker still holds a `JobCtx`.
    pub fn remove(&self, id: &str) -> Result<(), ()> {
        let mut inner = self.inner.write().expect("jobs lock");
        match inner.jobs.get(id) {
            Some(job) if job.is_finished() => {
                inner.jobs.remove(id);
                inner.order.retain(|existing| existing != id);
                Ok(())
            }
            Some(_) => Err(()),
            None => Err(()),
        }
    }

    // -----------------------------------------------------------------------
    // Internals
    // -----------------------------------------------------------------------

    fn insert(&self, name: String, status: JobStatus, announce: bool) -> (String, JobCtx) {
        let mut inner = self.inner.write().expect("jobs lock");
        inner.next += 1;
        let id = format!("job-{}", inner.next);
        let job = Job {
            id: id.clone(),
            name,
            status,
            progress: 0,
            logs: Vec::new(),
            started_ms: now_ms(),
            finished_ms: None,
            message: None,
            error: None,
            announce,
        };
        inner.jobs.insert(id.clone(), job);
        inner.order.push_back(id.clone());
        prune_locked(&mut inner);
        let ctx = JobCtx {
            id: id.clone(),
            inner: Arc::clone(&self.inner),
        };
        (id, ctx)
    }

    fn mark_running(&self, id: &str) {
        let mut inner = self.inner.write().expect("jobs lock");
        if let Some(job) = inner.jobs.get_mut(id) {
            job.status = JobStatus::Running;
        }
    }

    fn finish(
        &self,
        id: &str,
        status: JobStatus,
        message: Option<String>,
        error: Option<String>,
    ) {
        let finished = {
            let mut inner = self.inner.write().expect("jobs lock");
            match inner.jobs.get_mut(id) {
                Some(job) => {
                    job.status = status;
                    // With no explicit summary, the last line the worker logged
                    // is the most useful completion message.
                    job.message = message.or_else(|| job.logs.last().cloned());
                    job.error = error;
                    job.finished_ms = Some(now_ms());
                    job.progress = 100;
                    job.clone()
                }
                None => return,
            }
        };
        self.announce(&finished);
    }

    fn announce(&self, job: &Job) {
        // Interactive (tracked) operations already report their result to the
        // caller; only detached work raises a notification of its own.
        if !job.announce {
            return;
        }
        let Some(notifier) = &self.notifier else {
            return;
        };
        let request = match job.status {
            JobStatus::Success => NotificationRequest::info(
                JOB_EVENT,
                format!("{} finished", job.name),
                job.message.clone().unwrap_or_else(|| "done".to_string()),
            ),
            JobStatus::Failed => NotificationRequest::error(
                JOB_EVENT,
                format!("{} failed", job.name),
                job.error.clone().unwrap_or_else(|| "failed".to_string()),
            ),
            _ => return,
        };
        notifier.notify(request);
    }

    /// Prune at most once per [`PRUNE_INTERVAL_MS`], so the read path is not a
    /// writer on every poll.
    fn maybe_prune(&self) {
        let now = now_ms();
        if now.saturating_sub(self.last_prune_ms.load(Ordering::Relaxed)) < PRUNE_INTERVAL_MS {
            return;
        }
        self.last_prune_ms.store(now, Ordering::Relaxed);
        let mut inner = self.inner.write().expect("jobs lock");
        prune_locked(&mut inner);
    }
}

/// A worker's handle to its own job. Every method is a short lock-and-write, so
/// it is safe to call from a blocking worker or an async task.
#[derive(Clone)]
pub struct JobCtx {
    id: String,
    inner: Arc<RwLock<Inner>>,
}

impl JobCtx {
    /// Set completion percentage. Values are clamped to 0..=100.
    pub fn set_progress(&self, percent: u8) {
        let mut inner = self.inner.write().expect("jobs lock");
        if let Some(job) = inner.jobs.get_mut(&self.id) {
            job.progress = percent.min(100);
        }
    }

    /// Append one line to the job's log tail.
    pub fn log(&self, line: impl Into<String>) {
        let mut inner = self.inner.write().expect("jobs lock");
        if let Some(job) = inner.jobs.get_mut(&self.id) {
            job.logs.push(line.into());
            if job.logs.len() > MAX_LOGS {
                let excess = job.logs.len() - MAX_LOGS;
                job.logs.drain(0..excess);
            }
        }
    }

    /// Advance progress and log in one step, for "step 3 of 5" style updates.
    pub fn step(&self, percent: u8, line: impl Into<String>) {
        self.set_progress(percent);
        self.log(line);
    }
}

/// Settles a tracked job if the caller's future is dropped before it finishes.
///
/// Without this, an operation cancelled by the 30-second API deadline (a compose
/// pull routinely exceeds it) would leave its job `Running` forever, since a
/// non-finished job is neither removable nor prunable.
struct SettleGuard {
    manager: JobManager,
    id: String,
    settled: bool,
}

impl Drop for SettleGuard {
    fn drop(&mut self) {
        if !self.settled {
            self.manager.finish(
                &self.id,
                JobStatus::Failed,
                None,
                Some("cancelled before completion".to_string()),
            );
        }
    }
}

fn prune_locked(inner: &mut Inner) {
    let now = now_ms();
    // Any job that never settled (a wedged worker, or a leak that slipped past
    // the settle guard) is failed rather than counted as running forever.
    let stale_before = now.saturating_sub(STALE_JOB_MS);
    for job in inner.jobs.values_mut() {
        if !job.is_finished() && job.started_ms < stale_before {
            job.status = JobStatus::Failed;
            job.error = Some("stale: no completion was recorded".to_string());
            job.finished_ms = Some(now);
            job.progress = 100;
        }
    }

    let cutoff = now.saturating_sub(HISTORY_TTL_MS);
    // Drop finished jobs past their retention window. Collected first so the
    // map and the order list are not borrowed at the same time.
    let expired: Vec<String> = inner
        .order
        .iter()
        .filter(|id| match inner.jobs.get(*id) {
            Some(job) => job.is_finished() && job.finished_ms.unwrap_or(job.started_ms) < cutoff,
            None => true,
        })
        .cloned()
        .collect();
    for id in &expired {
        inner.jobs.remove(id);
    }
    if !expired.is_empty() {
        inner.order.retain(|id| !expired.contains(id));
    }

    // Then enforce the cap, oldest finished first.
    while inner.jobs.len() > MAX_JOBS {
        let victim = inner.order.iter().find_map(|id| {
            inner
                .jobs
                .get(id)
                .filter(|job| job.is_finished())
                .map(|_| id.clone())
        });
        match victim {
            Some(id) => {
                inner.jobs.remove(&id);
                inner.order.retain(|existing| existing != &id);
            }
            None => break,
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_records_success_and_progress() {
        let jobs = JobManager::new(None);
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let result = runtime.block_on(jobs.track_with("do a thing", |ctx| async move {
            ctx.step(50, "halfway");
            Ok::<_, String>("finished".to_string())
        }));
        assert_eq!(result.unwrap(), "finished");

        let job = jobs.list().into_iter().next().expect("a job");
        assert_eq!(job.name, "do a thing");
        assert_eq!(job.status, JobStatus::Success);
        assert_eq!(job.progress, 100);
        // With no explicit summary, the last logged line becomes the message.
        assert_eq!(job.message.as_deref(), Some("halfway"));
        assert_eq!(job.logs, vec!["halfway".to_string()]);
        assert!(job.is_finished());
    }

    #[test]
    fn track_records_failure_and_keeps_the_error() {
        let jobs = JobManager::new(None);
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let result = runtime.block_on(jobs.track_with("boom", |_ctx| async move {
            Err::<String, String>("it broke".to_string())
        }));
        assert_eq!(result.unwrap_err(), "it broke");

        let job = jobs.list().into_iter().next().expect("a job");
        assert_eq!(job.status, JobStatus::Failed);
        assert_eq!(job.error.as_deref(), Some("it broke"));
    }

    #[test]
    fn list_is_newest_first_and_remove_only_finished() {
        let jobs = JobManager::new(None);
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            jobs.track_with("one", |_ctx| async {
                Ok::<String, String>("a".to_string())
            })
            .await
            .unwrap();
            let second = jobs
                .track_with("two", |_ctx| async {
                    Ok::<String, String>("b".to_string())
                })
                .await
                .unwrap();
            assert_eq!(second, "b");
        });

        let listed = jobs.list();
        assert_eq!(listed[0].name, "two");
        assert_eq!(listed[1].name, "one");

        // Finished jobs can be cleared; unknown ids cannot.
        assert!(jobs.remove(&listed[0].id).is_ok());
        assert!(jobs.remove("nope").is_err());
        assert_eq!(jobs.list().len(), 1);
    }

    #[test]
    fn spawn_job_returns_an_id_before_it_finishes() {
        let jobs = JobManager::new(None);
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let id = runtime.block_on(async {
            let id = jobs.spawn_job("detached", |_ctx| Ok("ok".to_string()));
            // The worker runs on the blocking pool; give it a moment.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            id
        });
        let job = jobs.get(&id).expect("the job");
        assert_eq!(job.status, JobStatus::Success);
    }

    #[test]
    fn cap_drops_oldest_finished_jobs() {
        let jobs = JobManager::new(None);
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        for index in 0..(MAX_JOBS + 10) {
            runtime.block_on(
                jobs.track_with(format!("job {index}"), |_ctx| async {
                    Ok::<String, String>("done".to_string())
                }),
            )
            .unwrap();
        }
        assert!(jobs.list().len() <= MAX_JOBS);
    }
}
