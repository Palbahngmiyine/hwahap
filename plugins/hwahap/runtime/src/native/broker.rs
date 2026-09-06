use std::sync::Mutex;

use tokio::sync::oneshot;

use super::{
    json, load, save, timing, NativeCompletion, NativeDispatch, NativeRegistration, Pending,
};
use crate::error::{Error, Result};
use crate::state::Store;

mod catalog;
mod dispatch;

pub(super) struct Waiting {
    pub pending: Pending,
    pub sender: Option<oneshot::Sender<NativeCompletion>>,
}

/// A single-flight bridge: request is durable before the host can spawn an agent.
pub struct NativeSessions {
    pub(super) store: Store,
    pub(super) max_calls: u64,
    pub(super) timeout_secs: u64,
    pub(super) waiting: Mutex<Option<Waiting>>,
    pub(super) registered: tokio::sync::Notify,
    pub(super) host_session_id: Option<String>,
}

impl NativeSessions {
    pub fn new(store: Store, max_calls: u64, timeout_secs: u64) -> Self {
        Self {
            store,
            max_calls,
            timeout_secs,
            waiting: Mutex::new(None),
            registered: tokio::sync::Notify::new(),
            host_session_id: None,
        }
    }

    pub fn with_host_session_id(mut self, id: String) -> Self {
        self.host_session_id = Some(id);
        self
    }

    pub fn dispatch(&self) -> Result<Option<NativeDispatch>> {
        let guard = self.waiting.lock().map_err(poisoned)?;
        Ok(guard.as_ref().and_then(|w| {
            if w.pending.completion.is_some() {
                None
            } else {
                Some(w.pending.dispatch.clone())
            }
        }))
    }

    pub fn register(&self, registration: &NativeRegistration) -> Result<()> {
        if registration.agent_id.trim().is_empty() {
            return Err(Error::Rejected("native agent_id must not be empty".into()));
        }
        let mut guard = self.waiting.lock().map_err(poisoned)?;
        let waiting = guard
            .as_mut()
            .ok_or_else(|| Error::Rejected("no pending native dispatch".into()))?;
        let dispatch = &mut waiting.pending.dispatch;
        if registration.agent_id == "coordinator" && !dispatch.coordinator_allowed {
            return Err(Error::Rejected(
                "this role requires an independent native child".into(),
            ));
        }
        if dispatch.dispatch_id != registration.dispatch_id || dispatch.stop_required {
            return Err(Error::Rejected(
                "registration does not match an active dispatch".into(),
            ));
        }
        dispatch.verify_decision()?;
        if registration.decision_digest.as_deref() != Some(&dispatch.decision.digest) {
            return Err(Error::Rejected(
                "registration decision digest differs".into(),
            ));
        }
        super::pool::check_registration(&self.store, dispatch, &registration.agent_id)?;
        if waiting.pending.completion.is_some() {
            return Ok(());
        }
        match &dispatch.agent_id {
            Some(id) if id != &registration.agent_id => {
                return Err(Error::Rejected(
                    "native dispatch already belongs to another agent".into(),
                ));
            }
            // Retrying after an IO error must persist the identity, even if memory already has it.
            Some(_) => {}
            None => dispatch.agent_id = Some(registration.agent_id.clone()),
        }
        save(&self.store, &waiting.pending)?;
        super::pool::register(
            &self.store,
            &waiting.pending.dispatch,
            &registration.agent_id,
        )?;
        timing::observe(&self.store, &waiting.pending.dispatch, None, None)?;
        self.registered.notify_one();
        Ok(())
    }

    /// Durable recording precedes delivery. Identical retries do not deliver twice.
    pub fn complete(&self, completion: NativeCompletion) -> Result<()> {
        if !completion.agent_stopped {
            return Err(Error::Rejected(
                "wait for the native child to stop before completing".into(),
            ));
        }
        if let Some(usage) = &completion.reported_usage {
            usage.verify()?;
        }
        let mut guard = self.waiting.lock().map_err(poisoned)?;
        let waiting = guard
            .as_mut()
            .ok_or_else(|| Error::Rejected("no pending native dispatch".into()))?;
        let dispatch = &waiting.pending.dispatch;
        if completion.dispatch_id != dispatch.dispatch_id
            && Self::recorded_completion(&self.store, &completion)?
        {
            return Ok(());
        }
        if completion.dispatch_id != dispatch.dispatch_id
            || dispatch.agent_id.as_deref() != Some(&completion.agent_id)
            || dispatch.stop_required
        {
            return Err(Error::Rejected(
                "completion does not match the registered native dispatch".into(),
            ));
        }
        dispatch.verify_decision()?;
        if completion.decision_digest.as_deref() != Some(&dispatch.decision.digest) {
            return Err(Error::Rejected("completion decision digest differs".into()));
        }
        super::reply::result(&completion)?;
        if let Some(previous) = &waiting.pending.completion {
            if previous != &completion {
                return Err(Error::Rejected(
                    "native completion was already recorded with different content".into(),
                ));
            }
            if waiting.sender.is_none() {
                return Ok(());
            }
        }
        self.store.write_artifact(
            &format!("native-completion-{}.json", completion.dispatch_id),
            &json(&completion)?,
        )?;
        waiting.pending.completion = Some(completion.clone());
        save(&self.store, &waiting.pending)?;
        super::pool::stopped(&self.store, &waiting.pending.dispatch)?;
        timing::observe(
            &self.store,
            &waiting.pending.dispatch,
            Some("completed"),
            Some(completion.final_message.len() as u64),
        )?;
        let sender = waiting
            .sender
            .take()
            .ok_or_else(|| Error::Rejected("native continuation no longer exists".into()))?;
        sender.send(completion).map_err(|_| {
            Error::Rejected(
                "native continuation ended; stop acknowledgment is required before recovery".into(),
            )
        })
    }

    /// A lost response may be retried after the engine has already requested its next role.
    pub fn recorded_completion(store: &Store, completion: &NativeCompletion) -> Result<bool> {
        if completion.dispatch_id.len() != 64
            || !completion
                .dispatch_id
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
        {
            return Err(Error::Rejected("invalid native dispatch identifier".into()));
        }
        let path = store
            .artifacts_path()
            .join(format!("native-completion-{}.json", completion.dispatch_id));
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let prior: NativeCompletion =
                    serde_json::from_str(&text).map_err(|e| Error::Corrupt(e.to_string()))?;
                if &prior == completion {
                    Ok(true)
                } else {
                    Err(Error::Rejected(
                        "completion replay changed its recorded content".into(),
                    ))
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(Error::io(path, e)),
        }
    }

    /// Only after the engine has finished consuming the final result can its guard disappear.
    pub fn finish(&self) -> Result<()> {
        let guard = self.waiting.lock().map_err(poisoned)?;
        if guard
            .as_ref()
            .is_some_and(|w| w.pending.completion.is_none())
        {
            return Err(Error::Rejected(
                "native agent still needs stop acknowledgment".into(),
            ));
        }
        if load(&self.store)?.is_some_and(|pending| pending.completion.is_none()) {
            return Err(Error::Rejected(
                "persisted native dispatch still needs stop acknowledgment".into(),
            ));
        }
        super::clear(&self.store)
    }

    pub(super) fn expire(&self, outcome: &str) -> Result<()> {
        let mut guard = self.waiting.lock().map_err(poisoned)?;
        if let Some(waiting) = guard.as_mut() {
            waiting.pending.dispatch.stop_required = true;
            save(&self.store, &waiting.pending)?;
            timing::observe(&self.store, &waiting.pending.dispatch, Some(outcome), None)?;
        }
        Ok(())
    }

    pub(super) fn next_call(&self) -> Result<u64> {
        let count = match std::fs::read_dir(self.store.artifacts_path()) {
            Ok(files) => files
                .collect::<std::io::Result<Vec<_>>>()
                .map_err(|e| Error::io(self.store.artifacts_path(), e))?
                .iter()
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("native-request-")
                })
                .count() as u64,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            Err(e) => return Err(Error::io(self.store.artifacts_path(), e)),
        };
        if count >= self.max_calls {
            return Err(Error::ExecutionLimit(format!(
                "native call budget {} is exhausted; no agent was spawned",
                self.max_calls
            )));
        }
        if let Some(pending) = load(&self.store)? {
            if pending.completion.is_none() {
                return Err(Error::Rejected(
                    "unconsumed native agent must be stopped before another dispatch".into(),
                ));
            }
        }
        Ok(count + 1)
    }
}

impl crate::engine::Sessions for NativeSessions {
    fn preflight(&self, spec: &crate::session::SessionSpec) -> Result<()> {
        self.select(spec).map(|_| ())
    }

    fn run<'a>(
        &'a self,
        spec: &'a crate::session::SessionSpec,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<crate::session::SessionOutcome>> + Send + 'a>,
    > {
        Box::pin(self.execute(spec))
    }
}

fn poisoned<T>(_: std::sync::PoisonError<T>) -> Error {
    Error::Internal("native broker lock poisoned".into())
}
