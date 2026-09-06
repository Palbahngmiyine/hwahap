use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use super::{
    acknowledge_stopped, orphan, record_failure, resume_failed, NativeCompletion, NativeDispatch,
    NativeFailure, NativeRegistration, NativeResume, NativeSessions, NativeStopped, RepoLock,
};
use crate::config::Config;
use crate::engine::{Engine, StepOutcome};
use crate::error::{Error, Result};
use crate::state::Store;

#[derive(Default)]
pub struct NativeInput {
    pub abandon: Option<super::AbandonRequest>,
    pub host_observation: Option<crate::catalog::HostObservation>,
    pub verification_recovery: Option<crate::verification::Recovery>,
    pub approved_plan: Option<crate::approval::ApprovedPlanRequest>,
    pub plan_only: bool,
    pub build_confirmed: Option<String>,
    pub adjust_build: Option<crate::engine::AdjustBuildRequest>,
    pub question_response: Option<crate::dialogue::QuestionResponse>,
    pub recheck_pr: bool,
    pub build: Option<crate::engine::BuildRequest>,
    pub host_session_id: Option<String>,
    pub request: Option<String>,
    pub user_input: Option<String>,
    pub registration: Option<NativeRegistration>,
    pub completion: Option<NativeCompletion>,
    pub stopped: Option<NativeStopped>,
    pub dispatch_failure: Option<NativeFailure>,
    pub resume: Option<NativeResume>,
}

pub struct NativeProgress {
    pub outcome: StepOutcome,
    pub dispatch: Option<NativeDispatch>,
}

struct Active {
    broker: Arc<NativeSessions>,
    task: JoinHandle<Result<StepOutcome>>,
    // Registry and task both hold the lock, including while cancellation is being delivered.
    _lock: Arc<RepoLock>,
}

impl Drop for Active {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// One background continuation per canonical Git root. Polling never starts another writer.
#[derive(Default)]
pub struct NativeHost {
    active: Mutex<HashMap<PathBuf, Active>>,
}

impl NativeHost {
    /// Stop engine-owned commands before the MCP process exits; native children remain host-owned.
    pub async fn shutdown(&self) {
        let mut active = self.active.lock().await;
        for running in active.values() {
            running.task.abort();
        }
        for (_, mut running) in active.drain() {
            let _ = (&mut running.task).await;
        }
    }

    pub async fn advance(&self, root: &Path, input: NativeInput) -> Result<NativeProgress> {
        crate::approval::reject_unbound_implementation_request(input.request.as_deref())?;
        crate::approval::reject_unbound_implementation_request(input.user_input.as_deref())?;
        if input.request.is_some() && input.user_input.is_some() {
            return Err(Error::Rejected(
                "request and user_input cannot be combined; send one without discarding either message"
                    .into(),
            ));
        }
        let actions = usize::from(input.abandon.is_some())
            + usize::from(input.verification_recovery.is_some())
            + usize::from(input.build.is_some())
            + usize::from(input.approved_plan.is_some())
            + usize::from(input.build_confirmed.is_some())
            + usize::from(input.adjust_build.is_some())
            + usize::from(input.question_response.is_some())
            + usize::from(input.registration.is_some())
            + usize::from(input.completion.is_some())
            + usize::from(input.stopped.is_some())
            + usize::from(input.dispatch_failure.is_some())
            + usize::from(input.resume.is_some())
            + usize::from(input.recheck_pr);
        if actions > 1 || (actions > 0 && (input.request.is_some() || input.user_input.is_some())) {
            return Err(Error::Rejected(
                "send exactly one native action, without request or user_input".into(),
            ));
        }
        if input.plan_only && input.request.is_none() {
            return Err(Error::Rejected(
                "plan_only is a start option and requires request".into(),
            ));
        }
        let mut active = self.active.lock().await;
        let store = Store::open(root)?;
        if let Some(scope) = &input.host_session_id {
            if scope.trim().is_empty() || scope.len() > 128 {
                return Err(Error::Rejected("host_session_id must be a stable, nonempty parent task identifier (at most 128 bytes)".into()));
            }
        }
        // Recover the compound approval snapshot before deciding who owns a new run.
        if !active.contains_key(root) {
            let _lock = RepoLock::acquire(root)?;
            store.recover()?;
        }
        if let Some(request) = &input.abandon {
            if let Some(done) = super::abandon::archived(&store, request)? {
                return Ok(done);
            }
        }
        if let Some(ack) = &input.stopped {
            if let Some(done) =
                super::abandon::replay_stop(&store, ack, input.host_session_id.as_deref())?
            {
                return Ok(done);
            }
        }
        let expected_scope = input
            .host_session_id
            .clone()
            .or(store.read_run()?.map(|run| run.run_id));
        if let Some(run) = store
            .read_run()?
            .filter(|r| !(r.state.is_terminal() && input.request.is_some()))
        {
            let wanted = serde_json::json!({"run_id":run.run_id,"pool_scope":expected_scope});
            let saved =
                crate::pr_review::read_evidence::<serde_json::Value>(&store, "native-owner.json")?;
            if saved.as_ref().is_some_and(|v| v != &wanted) {
                return Err(Error::Rejected(
                    "native work belongs to another parent task".into(),
                ));
            }
            if saved.is_none() {
                let _lock = if active.contains_key(root) {
                    None
                } else {
                    Some(RepoLock::acquire(root)?)
                };
                // BUILD seals its parent before creating the worktree, including interrupted starts.
                if crate::pr_review::read_evidence::<serde_json::Value>(
                    &store,
                    "build-request.json",
                )?
                .and_then(|v| v["pool_scope"].as_str().map(str::to_owned))
                .is_some_and(|scope| Some(scope) != expected_scope)
                {
                    return Err(Error::Rejected(
                        "BUILD belongs to another parent task".into(),
                    ));
                }
                if store
                    .read_events()?
                    .iter()
                    .rev()
                    .find(|e| {
                        e.kind == "approved_plan_snapshot" && e.data["run"]["run_id"] == run.run_id
                    })
                    .and_then(|e| e.data["pool_scope"].as_str())
                    .is_some_and(|scope| Some(scope) != expected_scope.as_deref())
                {
                    return Err(Error::Rejected(
                        "approved plan belongs to another parent task".into(),
                    ));
                }
                crate::pr_review::save_evidence(&store, "native-owner.json", &wanted)?;
            }
        }
        if active
            .get(root)
            .is_some_and(|running| running.broker.host_session_id != input.host_session_id)
            || orphan(&store)?.is_some_and(|dispatch| Some(dispatch.pool_scope) != expected_scope)
        {
            return Err(Error::Rejected(
                "native work belongs to another parent task; do not reuse or stop its agents"
                    .into(),
            ));
        }
        if let Some(request) = &input.abandon {
            let lock = if let Some(running) = active.get(root) {
                running._lock.clone()
            } else {
                Arc::new(RepoLock::acquire(root)?)
            };
            super::abandon::request(&store, request)?;
            if let Some(mut running) = active.remove(root) {
                running.task.abort();
                let _ = (&mut running.task).await;
            }
            let result = super::abandon::finish(&store, request);
            drop(lock);
            return result;
        }
        if input.stopped.is_none() && input.verification_recovery.is_none() {
            if let Some(request) = super::abandon::pending(&store)? {
                let _lock = RepoLock::acquire(root)?;
                return super::abandon::finish(&store, &request);
            }
        }
        let observed = input.host_observation.clone();
        let observing_parent = input.host_session_id.as_deref();
        let starts =
            input.request.is_some() || input.build.is_some() || input.approved_plan.is_some();
        let observation_error = observed.as_ref().and_then(|observation| {
            let _lock = if active.contains_key(root) {
                None
            } else {
                match RepoLock::acquire(root) {
                    Ok(lock) => Some(lock),
                    Err(error) => return Some(error),
                }
            };
            let result = match observing_parent {
                None => Err(Error::Rejected(
                    "host_observation requires host_session_id".into(),
                )),
                Some(parent) if starts => {
                    use crate::clock::Clock;
                    observation.validate(parent, &crate::clock::SystemClock.now())
                }
                Some(parent) => crate::catalog::host::observe(
                    &store,
                    &crate::clock::SystemClock,
                    parent,
                    observation,
                ),
            };
            result.err()
        });
        let mut observation_error = observation_error;
        if observation_error.is_some() && (input.completion.is_some() || input.stopped.is_some()) {
            let _lock = if active.contains_key(root) {
                None
            } else {
                Some(RepoLock::acquire(root)?)
            };
            store.append_event(
                &crate::clock::SystemClock,
                "host_observation_required",
                serde_json::json!({"reason":"rejected companion observation"}),
            )?;
        }
        if input.completion.is_none() && input.stopped.is_none() {
            if let Some(error) = observation_error.take() {
                return Err(error);
            }
        }
        if let Some(recovery) = &input.verification_recovery {
            if active.contains_key(root) || orphan(&store)?.is_some() {
                return Err(Error::Rejected(
                    "stop active native work before verification recovery".into(),
                ));
            }
            let _lock = RepoLock::acquire(root)?;
            crate::verification::acknowledge_interrupted(
                &store,
                &crate::clock::SystemClock,
                recovery,
            )?;
            return Ok(NativeProgress {
                outcome: Engine::open(root)?.status()?,
                dispatch: None,
            });
        }
        if input.build_confirmed.is_some()
            || input.approved_plan.is_some()
            || input.adjust_build.is_some()
            || input.question_response.is_some()
        {
            if active.contains_key(root) || orphan(&store)?.is_some() {
                return Err(Error::Rejected(
                    "finish or recover native work before changing stages".into(),
                ));
            }
            let _lock = RepoLock::acquire(root)?;
            let engine = Engine::open(root)?;
            let outcome = if let Some(approved) = &input.approved_plan {
                let outcome = engine
                    .register_approved_plan_for_parent(approved, expected_scope.as_deref())?;
                if let Some(observation) = &observed {
                    crate::catalog::host::observe(
                        &store,
                        &crate::clock::SystemClock,
                        &observation.host_session_id,
                        observation,
                    )?;
                }
                crate::pr_review::save_evidence(
                    &store,
                    "native-owner.json",
                    &serde_json::json!({
                        "run_id":outcome.run_id, "pool_scope":expected_scope.as_ref().unwrap_or(&outcome.run_id)
                    }),
                )?;
                outcome
            } else if let Some(digest) = &input.build_confirmed {
                engine.build_confirmed(digest)?
            } else if let Some(response) = &input.question_response {
                engine.answer_questions(response)?
            } else {
                engine.adjust_build(input.adjust_build.as_ref().expect("checked action"))?
            };
            return Ok(NativeProgress {
                outcome,
                dispatch: None,
            });
        }
        if input.recheck_pr {
            if active.contains_key(root) || orphan(&store)?.is_some() {
                return Err(Error::Rejected(
                    "finish or recover native work before PR recheck".into(),
                ));
            }
            let _lock = RepoLock::acquire(root)?;
            return Ok(NativeProgress {
                outcome: Engine::open(root)?.recheck_pr()?,
                dispatch: None,
            });
        }
        if let Some(build) = &input.build {
            if active.contains_key(root) || orphan(&store)?.is_some() {
                return Err(Error::Rejected(
                    "native execution must finish before direct BUILD".into(),
                ));
            }
            let _lock = RepoLock::acquire(root)?;
            let outcome = Engine::open(root)?
                .start_build_for_parent(build, input.host_session_id.as_deref())?;
            if let Some(observation) = &observed {
                crate::catalog::host::observe(
                    &store,
                    &crate::clock::SystemClock,
                    &observation.host_session_id,
                    observation,
                )?;
            }
            crate::pr_review::save_evidence(
                &store,
                "native-owner.json",
                &serde_json::json!({
                    "run_id":outcome.run_id, "pool_scope":input.host_session_id.as_ref().unwrap_or(&outcome.run_id)
                }),
            )?;
            return Ok(NativeProgress {
                outcome,
                dispatch: None,
            });
        }
        if let Some(failure) = &input.dispatch_failure {
            super::failure::check_failure(&store, failure)?;
            let lock = if let Some(mut running) = active.remove(root) {
                running.task.abort();
                let _ = (&mut running.task).await;
                running._lock.clone()
            } else {
                Arc::new(RepoLock::acquire(root)?)
            };
            let dispatch = record_failure(&store, failure)?;
            drop(lock);
            return progress(root, Some(dispatch), false);
        }
        if let Some(resume) = &input.resume {
            if let Some(running) = active.get(root) {
                if super::failure::recorded_resume(&store, resume)? {
                    return progress(root, running.broker.dispatch()?, true);
                }
                return Err(Error::Rejected("native execution is still active".into()));
            }
            let _lock = RepoLock::acquire(root)?;
            resume_failed(&store, resume)?;
            return progress(root, orphan(&store)?, false);
        }
        if let Some(ack) = &input.stopped {
            if ack.dispatch_id.len() != 64
                || !ack.dispatch_id.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err(Error::Rejected("invalid stop dispatch ID".into()));
            }
            if orphan(&store)?.is_none() {
                if let Some(request) = super::abandon::pending(&store)? {
                    let path = store
                        .artifacts_path()
                        .join(format!("native-stopped-{}.json", ack.dispatch_id));
                    let saved: super::NativeStopped = serde_json::from_slice(
                        &std::fs::read(&path).map_err(|e| Error::io(&path, e))?,
                    )
                    .map_err(|e| Error::Corrupt(e.to_string()))?;
                    if serde_json::to_value(&saved).unwrap() != serde_json::to_value(ack).unwrap() {
                        return Err(Error::Rejected("stop replay differs".into()));
                    }
                    let _lock = RepoLock::acquire(root)?;
                    return super::abandon::finish(&store, &request);
                }
            }
            super::check_stopped(&store, ack)?;
            let lock = if let Some(mut running) = active.remove(root) {
                running.task.abort();
                let _ = (&mut running.task).await;
                running._lock.clone()
            } else {
                Arc::new(RepoLock::acquire(root)?)
            };
            acknowledge_stopped(&store, ack)?;
            if let Some(error) = observation_error.take() {
                return Err(error);
            }
            if let Some(request) = super::abandon::pending(&store)? {
                return super::abandon::finish(&store, &request);
            }
            drop(lock);
            return progress(root, None, false);
        }
        if let Some(running) = active.get(root) {
            if input.request.is_some() || input.user_input.is_some() {
                return Err(Error::Rejected(
                    "a native continuation is active; stop it before changing its input".into(),
                ));
            }
            if let Some(registration) = &input.registration {
                running.broker.register(registration)?;
            }
            if let Some(completion) = input.completion {
                running.broker.complete(completion)?;
                if let Some(error) = observation_error.take() {
                    return Err(error);
                }
            }
        } else {
            let lock = Arc::new(RepoLock::acquire(root)?);
            if let Some(dispatch) = orphan(&store)? {
                if actions > 0 || input.request.is_some() || input.user_input.is_some() {
                    return Err(Error::Rejected(
                        "pending native work requires recovery before new input".into(),
                    ));
                }
                return progress(root, Some(dispatch), false);
            }
            if input.registration.is_some() {
                return Err(Error::Rejected(
                    "no live native dispatch accepts registration".into(),
                ));
            }
            if let Some(completion) = input.completion {
                if NativeSessions::recorded_completion(&store, &completion)? {
                    if let Some(error) = observation_error.take() {
                        return Err(error);
                    }
                    return progress(root, None, false);
                }
                return Err(Error::Rejected(
                    "no live native dispatch accepts completion".into(),
                ));
            }
            let config = Config::for_run(&store)?;
            let start_store = store.clone();
            let start_parent = input.host_session_id.clone();
            let mut sessions =
                NativeSessions::new(store, config.native_max_calls, config.native_timeout_secs);
            if let Some(scope) = input.host_session_id {
                sessions = sessions.with_host_session_id(scope);
            }
            let broker = Arc::new(sessions);
            let engine = Engine::open(root)?;
            let sessions = broker.clone();
            let task_lock = lock.clone();
            let task = tokio::spawn(async move {
                let _lock = task_lock;
                if let Some(request) = input.request.as_deref() {
                    let outcome = engine.start_planning(request, input.plan_only)?;
                    if let Some(observation) = &observed {
                        crate::catalog::host::observe(
                            &start_store,
                            &crate::clock::SystemClock,
                            &observation.host_session_id,
                            observation,
                        )?;
                    }
                    crate::pr_review::save_evidence(
                        &start_store,
                        "native-owner.json",
                        &serde_json::json!({
                            "run_id":outcome.run_id,
                            "pool_scope":start_parent.as_ref().unwrap_or(&outcome.run_id)
                        }),
                    )?;
                    return Ok(outcome);
                }
                engine
                    .step_with(
                        &*sessions,
                        input.request.as_deref(),
                        input.user_input.as_deref(),
                    )
                    .await
            });
            active.insert(
                root.into(),
                Active {
                    broker,
                    task,
                    _lock: lock,
                },
            );
        }
        tokio::task::yield_now().await;
        if active
            .get(root)
            .is_some_and(|running| running.task.is_finished())
        {
            let mut running = active.remove(root).expect("active entry was just checked");
            let result = (&mut running.task)
                .await
                .map_err(|e| Error::Internal(format!("native engine task ended: {e}")))?;
            if let Some(dispatch) = running.broker.dispatch()? {
                return progress(
                    root,
                    Some(NativeDispatch {
                        stop_required: true,
                        ..dispatch
                    }),
                    false,
                );
            }
            running.broker.finish()?;
            return Ok(NativeProgress {
                outcome: result?,
                dispatch: None,
            });
        }
        let dispatch = active
            .get(root)
            .map(|running| running.broker.dispatch())
            .transpose()?
            .flatten();
        progress(root, dispatch, true)
    }

    pub async fn status(&self, root: &Path) -> Result<NativeProgress> {
        let active = self.active.lock().await;
        match active.get(root) {
            Some(running) => progress(root, running.broker.dispatch()?, true),
            None => progress(root, orphan(&Store::open(root)?)?, false),
        }
    }

    pub async fn ship(&self, root: &Path, confirmation: &str) -> Result<StepOutcome> {
        let active = self.active.lock().await;
        if active.contains_key(root) {
            return Err(Error::Rejected("native execution is still active".into()));
        }
        let _lock = RepoLock::acquire(root)?;
        if let Some(dispatch) = orphan(&Store::open(root)?)? {
            return Err(Error::Rejected(
                if dispatch
                    .failure
                    .as_ref()
                    .is_some_and(|f| f.no_agent_created)
                {
                    "native execution is paused; resume with observed host recovery before shipping"
                        .into()
                } else {
                    "native stop acknowledgment is required before shipping".into()
                },
            ));
        }
        Engine::open(root)?.ship(confirmation)
    }
}

fn progress(
    root: &Path,
    dispatch: Option<NativeDispatch>,
    running: bool,
) -> Result<NativeProgress> {
    let mut outcome = Engine::open(root)?.status()?;
    if let Some(dispatch) = &dispatch {
        outcome.next = if dispatch.stop_required {
            "native_stop"
        } else if dispatch.failure.is_some() {
            "native_paused"
        } else if dispatch.agent_id.is_some() {
            "native_wait"
        } else {
            "native_dispatch"
        }
        .into();
        outcome.message = if let Some(failure) = &dispatch.failure {
            format!(
                "Native dispatch failed: {}. {}",
                failure.message,
                if failure.no_agent_created {
                    "No child was created. Do not poll or spawn again; resume only with new observed host recovery evidence. The run and plan are preserved."
                } else {
                    "Child creation is uncertain. Locate the exact dispatch and stop all its work before acknowledging recovery. Do not spawn again."
                }
            )
        } else if dispatch.stop_required {
            "Stop this native agent and all remaining commands, then acknowledge the exact dispatch before recovery.".into()
        } else {
            format!("Native {} dispatch {}", dispatch.role, dispatch.dispatch_id)
        };
        if !dispatch.stop_required && dispatch.failure.is_none() {
            if let Some(elapsed) =
                super::timing::elapsed_since_offer(&Store::open(root)?, &dispatch.dispatch_id)?
            {
                outcome.message.push_str(&format!(
                    ". Host-observed elapsed: {elapsed} ms; target {}s, deadline {}s.",
                    dispatch.soft_budget_secs, dispatch.hard_timeout_secs,
                ));
                if elapsed > dispatch.soft_budget_secs.saturating_mul(1000) {
                    outcome.message.push_str(" Target exceeded: inspect progress and remaining scope; do not infer completion or start another agent.");
                }
            }
        }
    } else if running {
        outcome.next = "native_wait".into();
        outcome.message =
            "Hwahap is validating or preparing the next native dispatch; poll after one second."
                .into();
    }
    Ok(NativeProgress { outcome, dispatch })
}
