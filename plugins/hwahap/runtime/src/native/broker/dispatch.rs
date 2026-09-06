use std::io::Read;
use std::time::{Duration, Instant};

use tokio::sync::oneshot;

use super::{NativeSessions, Waiting};
use crate::canonical::Digest;
use crate::error::{Error, Result};
use crate::git::Git;
use crate::native::{json, save, timing, NativeDispatch, NativeLane, Pending};
use crate::session::{
    access_for, Access, NativeReceipt, SessionOutcome, SessionReceipt, SessionSpec,
};

impl NativeSessions {
    pub async fn execute(&self, spec: &SessionSpec) -> Result<SessionOutcome> {
        let started = Instant::now();
        let wanted = self.profiles.for_role(spec.role).clone();
        let run = self
            .store
            .read_run()?
            .ok_or_else(|| Error::Rejected("native dispatch has no active run".into()))?;
        let git = Git::open(&spec.cwd)?;
        let base_head = git.run_in(&spec.cwd, &["rev-parse", "HEAD"])?;
        let mut entropy = [0u8; 32];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(&mut entropy))
            .map_err(|e| Error::io("/dev/urandom", e))?;
        let dispatch_id = hex::encode(entropy);
        let access = match access_for(spec.role) {
            Access::ReadOnly => "read_only",
            Access::WorkspaceWrite => "workspace_write",
        };
        let lane = if wanted.model == "gpt-6-astra"
            && matches!(
                spec.role,
                crate::profile::Role::FactFinder | crate::profile::Role::Implementer
            ) {
            NativeLane::Coordinator
        } else {
            NativeLane::for_role(spec.role)
        };
        if lane == NativeLane::Coordinator && wanted.model != "gpt-6-astra" {
            return Err(Error::Rejected(
                "native planning and repair require an Astra parent and profiles.deep.model=gpt-6-astra".into(),
            ));
        }
        let soft_budget_secs = timing::soft_budget(spec.role).min(self.timeout_secs);
        let hard_timeout_secs = self.timeout_secs;
        let pool_scope = self
            .host_session_id
            .clone()
            .unwrap_or_else(|| run.run_id.clone());
        let brief =
            format!(
            "Hwahap dispatch {dispatch_id}. You are a native worker for role {}. Work only in the absolute directory {}. \
             This request grants {} access by instruction; it does not establish an OS sandbox. \
             Do not spawn, delegate to, or resume other agents. Do not edit Hwahap state, the host \
             checkout, or any path outside the requested working directory. Do not run the Hwahap \
             skill or tools recursively. Stop all commands before returning a final answer. \
             Aim to finish this role within {soft_budget_secs}s; the hard deadline is {hard_timeout_secs}s. \
             Keep investigation scoped. If blocked, report the concrete blocker in the result contract; \
             never claim a pass or completion merely because the time budget is ending. \
             New children start without inherited parent history. Reused children keep their lane \
             and must treat this brief and current repository as authoritative. The Astra coordinator \
             performs author-side Deep roles; independent reviewers never write.\n\n{}\n\n\
             Native transport: wrap the result object above as {{\"dispatch_id\":\"{dispatch_id}\",\"result\":<result object>}}. \
             Return that single JSON envelope, not a prior turn's answer.",
            spec.role.as_str(), spec.cwd.display(), access, spec.prompt
        );
        let mut dispatch = NativeDispatch {
            dispatch_id: dispatch_id.clone(),
            run_id: run.run_id,
            role: spec.role.as_str().into(),
            profile: spec.role.profile().as_str().into(),
            unit: spec.unit.clone(),
            model: wanted.model.clone(),
            effort: wanted.effort.as_str().into(),
            cwd: spec.cwd.to_string_lossy().into_owned(),
            access: access.into(),
            coordinator_allowed: wanted.model == "gpt-6-astra" && lane == NativeLane::Coordinator,
            prompt_digest: Digest::of_bytes(brief.as_bytes()).to_string(),
            plan_digest: run.plan_digest.map(|digest| digest.to_string()),
            base_head,
            brief,
            agent_id: None,
            stop_required: false,
            failure: None,
            pool_scope,
            lane,
            reuse_agent_id: None,
            soft_budget_secs,
            hard_timeout_secs,
        };
        dispatch.reuse_agent_id = crate::native::pool::reusable(&self.store, &dispatch)?;
        let (sender, receiver) = oneshot::channel();
        {
            let mut guard = self.waiting.lock().map_err(super::poisoned)?;
            self.next_call()?;
            timing::begin(
                &self.store,
                &dispatch_id,
                spec.role,
                dispatch.brief.len() as u64,
                soft_budget_secs,
                hard_timeout_secs,
            )?;
            let pending = Pending {
                dispatch,
                completion: None,
            };
            // Request is immutable evidence; pending also exists before anything is exposed.
            self.store.write_artifact(
                &format!("native-request-{dispatch_id}.json"),
                &json(&pending.dispatch)?,
            )?;
            save(&self.store, &pending)?;
            *guard = Some(Waiting {
                pending,
                sender: Some(sender),
            });
        }
        let completion = match tokio::time::timeout(
            Duration::from_secs(self.timeout_secs),
            receiver,
        )
        .await
        {
            Ok(Ok(completion)) => completion,
            result => {
                let outcome = if result.is_err() {
                    "deadline"
                } else {
                    "channel_closed"
                };
                self.expire(outcome)?;
                let reason = if result.is_err() {
                    "deadline elapsed"
                } else {
                    "continuation channel closed"
                };
                return Err(Error::Rejected(format!("native {reason}; stop the child and its commands before acknowledging recovery")));
            }
        };
        let final_message = crate::native::reply::result(&completion)?;
        Ok(SessionOutcome {
            transcript: completion.final_message.clone(),
            final_message,
            receipt: SessionReceipt::Native(NativeReceipt {
                dispatch_id,
                agent_id: completion.agent_id,
                profile: spec.role.profile(),
                role: spec.role,
                unit: spec.unit.clone(),
                model_requested: wanted.model,
                effort_requested: wanted.effort,
                elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                reported_usage: completion.reported_usage,
            }),
            stop_reason: "native_agent_stopped".into(),
        })
    }
}
