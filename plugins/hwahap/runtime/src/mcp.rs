//! The MCP surface: exactly three tools, and the instructions that govern them.
//!
//! The host sees `hwahap_step`, `hwahap_status`, and `hwahap_ship` and nothing else. There is no
//! `plan`, `cycle`, `retry`, `create_unit`, `spawn_worker`, or `integrate` tool, because every one
//! of those would hand a scheduling or approval decision back to the calling model. The state
//! machine decides; the host executes the explicit native dispatch protocol.
//!
//! [`INSTRUCTIONS`] is the single source of the cross-tool protocol. The skill file does not repeat
//! it, and neither does any reference document: one rule, one place.

use std::path::PathBuf;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ErrorData, Json, ServerHandler};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::engine::StepOutcome;
use crate::error::Error;
use crate::git::Git;
use crate::native::{
    NativeCompletion, NativeDispatch, NativeFailure, NativeHost, NativeInput, NativeProgress,
    NativeRegistration, NativeResume, NativeStopped,
};

/// The cross-tool protocol, returned in the MCP `initialize` response.
///
/// The first paragraph stands alone: a host that reads only the opening of this string still learns
/// the loop it must run and the one thing it must never do.
pub const INSTRUCTIONS: &str = "\
Hwahap turns an implementation request into a confirmed plan and then builds it autonomously. Call \
hwahap_step and follow its `next` field: `continue` means call hwahap_step again immediately \
without asking the user; `repair_translation` means repair and resubmit approved_plan as described below; \
`await_user` means show `message` and wait; `completed` and `blocked` mean \
show `message` and stop. Pass the user's reply verbatim in `user_input`. Never compose, complete, \
or infer a CONFIRM PLAN or SHIP line on the user's behalf — only the user may type one.

For an already approved plan, use approved_plan with the verbatim implementation_request, approved \
markdown, SHA-256 markdown_digest, inspected source_head and executable contract. Inline handoff accepts \
PLEASE IMPLEMENT THIS PLAN: plus the full plan. A referenced handoff uses approval.reference with the \
actual source_reference, disposition, plan_digest and implementation_request_digest. Preserve the user's \
approved/not_approved/rejected/cancelled state; only approved proceeds. These are host-observed bindings. \
For an unexecuted draft, include its current replaces_plan_digest. The runtime retains source approval, \
independently reviews the translation and enters BUILD. Repairing translation preserves that approval. \
Ask only about new material choices. Relay actual user messages and approval evidence. SHIP is separate.

For PLAN alone, start with request and plan_only:true. Confirmation saves plan_ready without \
implementation or GitHub authentication. Default plan_only:false continues from confirmed PLAN to BUILD. \
When the user later requests BUILD of that saved plan, send build_confirmed with its full plan_digest, \
never reconstruct a direct-build contract. A stale source requires reopening PLAN. For ADJUST, use \
adjust_build with the verbatim user_instruction, current contract_digest and affected unit_ids only \
when correcting implementation under unchanged acceptance, tests and paths. Any contract change or \
uncertain routing returns to PLAN through user_input. Both paths re-review the updated draft before SHIP.

When question_batch is present, present its exact question bodies and all option labels using the \
host's actually available request_user_input or request_user_input_async capability. request_user_input \
may require Codex Plan mode; do not call an unavailable tool, invent AskUserQuestion, or switch modes. \
Prefer the asynchronous question UI when it can display every option. Map question to title and \
option labels to options; descriptions belong only in a supported option-description field. \
Keep choices out of the question body. Keep rationale, sources and confidence in .hwahap/plan.md; \
link that document when detail is useful. Show the question card once, without repeating the \
question batch or full run message in commentary. Never shorten away alternatives. \
If the UI cannot represent every option, use a free-text question showing all full labels; if no \
question tool is available, show that same complete page in the conversation. Relay actual answers \
as question_response:{batch_id,responses:[{id,answer}]} with the unchanged batch ID and answer text. \
Do not translate labels into C= directives or infer missing choices. Merely preselecting a default, timeout, \
cancel, or request-resolved event is not a submitted answer. Accept an actual user-submitted response \
payload, including a submitted recommended option; otherwise remain waiting. \
The engine pages the whole ready frontier, then rechecks implications. Free text is unconfirmed until \
the user chooses a clarified interpretation. CONFIRM PLAN and SHIP still require the user's exact typed \
line in user_input/confirmation; never manufacture them from a question UI response.

Only when the user explicitly requests execution without planning, send build instead of request. \
Its user_instruction must be that user's exact authorization; specify the objective, new codex/ \
branch, remote base branch, scoped units with observable acceptance and test commands, and full_suite. \
Direct BUILD selects a qualified worker or parent from the task assessment and catalog, with independent \
Critic/Auditor lanes secured before authorship. It records the explicit BUILD instruction. \
Requests without an already-approved Codex plan still use the planning and confirmation flow. Never infer direct BUILD permission. \
Every BUILD publishes a draft before independent attack and defense. Confirmed findings go to \
parent repair; both teams review the changed commit. Use recheck_pr:true alone to revalidate this \
run's existing draft after a runtime upgrade; it preserves the contract and retry budget.

Use a qualified parent coordinator and the same host_session_id for the run. Supply host_observation \
from actual host metadata: parent model/effort, available models/efforts/tools, free slots, observation \
time and source. Refresh when requested; the maximum age is 300 seconds. The run pins its catalog \
revision and model/effort/role assignments. Catalog replacement applies to new runs; an unavailable \
bound model pauses its existing run for recovery. Worker, Critic and Auditor retain distinct identities. \
Use the host's native spawn, follow-up, wait and interrupt capabilities.

For `native_dispatch`, follow the exact lane and identity. If lane=coordinator, register \
agent_id=coordinator and execute the brief in this qualified parent (planning, implementation or repair). Never spawn \
a fourth child for that lane. If reuse_agent_id is present, FIRST register that exact agent ID, \
then send the exact brief with the native follow-up tool ONCE. Registration is durable before \
follow-up so a lost response cannot trigger duplicate delivery. If reuse_agent_id is absent, \
spawn one child with task_name=hwahap_<dispatch_id>, fork_turns=none, requested model/effort and \
exact brief, then register its returned ID immediately. Never replace a retained child by spawning \
another, change its model, or use it in another lane.

For `delegation_wait`, read the reason and supply task_assessment with the current run_id, contract_digest, \
unit, role, requirements, three risk ratings, topology and source evidence. A task profile is shared by \
all roles for one unit, or by all run-level roles; role requirements are merged by the runtime. Put unit \
profiles and an aggregate run profile in plan.task_profiles. Use the unit ID as writer_owner, or run_id \
for aggregate writes, and exact approved write_paths. Refresh assessments after contract changes. \
High-risk profiles include an authorized disposable checkout and failure/recovery test commands; the \
runtime requests two independent preflight reviews and executes both checks before author dispatch. \
Copy native_dispatch.decision.digest into decision_digest for both registration and completion. \
Keep the offered identity, model, effort and lane throughout that dispatch.

Registered progress omits brief text and returns native_brief with the immutable request artifact and \
prompt digest. Retain the first offered brief, or read .hwahap/artifacts/<artifact> in the run repository. \
The handoff deadline bounds registration; execution gets its own deadline after registration. \
For `native_wait`, coordinator means perform the assigned work here, not wait for a child. \
Otherwise use event-driven native waits of at most 30 seconds and check hwahap_step after a wait \
expires; never sleep for 360 seconds or hold one blocking wait through the deadline. When the \
engine alone is validating, poll after one second. Return completion only after the child turn \
and its commands stop, relaying its exact final text with dispatch_id, agent_id, agent_stopped:true \
and reported_usage:null unless real tool counters exist. The brief requires a dispatch_id/result \
JSON envelope; every reply without the current ID is rejected. Keep completed pool children \
for later follow-up turns. Do not close them after each result; interruption is not thread release. \
Requested model/access and reported tokens are not independent applied-model, sandbox or billing proof.

Report a refused spawn or unavailable capability through dispatch_failure with the exact error. \
Use no_agent_created:true only when no child exists and no follow-up was attempted. For uncertain \
creation or any failed follow-up, use false. Never retry a delivery after an ambiguous response. \
For `native_stop`, stop the registered child (or the reuse_agent_id, or the exact hwahap_<dispatch_id> \
child if unregistered) and all its commands, then include its discovered agent ID in the stopped \
acknowledgment so recovery retains that child. Use a null ID only after confirming no child exists. Never \
acknowledge an uncertain stop. Missing retained agents are a blocker, not permission to reuse \
a different lane or create replacements.

For `native_paused`, show the failure and stop polling/spawning. Preserve this run. Resume once \
with dispatch_id and new observed host recovery evidence; elapsed time or reworded old evidence \
is not recovery. New dispatches still spend native_max_calls. Do not change global thread limits, \
close unrelated tasks, fabricate results or launch an ACP/CLI replacement. Hwahap owns code edits, \
tests, commits and PRs; outside the exact dispatch, do not perform that work independently. \
Host-side `hwahap usage attach <repo> <session.jsonl>` and `usage sync <repo>` are allowed for \
local token observation in .hwahap/usage.json; see USAGE.md. Attach parent and retained children \
before their first work in this run. Missing counters remain unknown; never invent reported_usage. \
The auditor is always a separate child that never participates in implementation.

For unresolved verification, inspect its `verification_process` journal event and confirm the \
owned command and runtime have stopped. Submit `verification_recovery` with the exact run_id, \
verification_id, all_work_stopped:true and observed stop evidence. Missing completion remains \
unsuccessful; the next step reruns the command. Never stop an unrelated process.

For ordinary PLAN, `CONFIRM PLAN <challenge>` freezes the plan. An approved Codex plan import \
retains its actual implementation request instead; never fabricate that CONFIRM PLAN line. After \
valid approval, continue within scope without duplicate BUILD approval. `SHIP <challenge>` marks the finished draft \
pull request ready for review. Both challenges are printed by Hwahap and are bound to exact \
content, so a challenge that does not match is rejected rather than corrected.

hwahap_status reads the run and changes nothing. hwahap_ship is the only consequential action, and \
it refuses unless the user typed the exact SHIP line, the pull request head is unchanged, required \
checks pass, and the final review is still fresh.";

/// Arguments to `hwahap_step`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct StepArgs {
    /// Include full cost evidence; the default returns a bounded summary and artifact reference.
    #[serde(default)]
    pub include_cost_evidence: bool,
    /// Optional host-provided session log, attached with a current-usage baseline.
    #[serde(default)]
    pub usage_session_path: Option<String>,
    /// Wait for engine work inside this call, at most 30 seconds. Zero returns immediately.
    #[serde(default = "default_wait_ms")]
    pub wait_ms: u64,
    /// Parent assessment bound to the current task and contract.
    #[serde(default)]
    pub task_assessment: Option<crate::delegation::TaskAssessment>,
    /// End the named run, preserving its worktree and evidence.
    #[serde(default)]
    pub abandon: Option<crate::native::AbandonRequest>,
    /// Current host inventory, accompanying one native action.
    #[serde(default)]
    pub host_observation: Option<crate::catalog::HostObservation>,
    #[serde(default)]
    pub verification_recovery: Option<crate::verification::Recovery>,
    /// Exact Codex plan implementation request and its executable translation, reviewed before BUILD.
    #[serde(default)]
    pub approved_plan: Option<crate::approval::ApprovedPlanRequest>,
    /// Actual answers to the current question_batch, relayed without rewriting their text.
    #[serde(default)]
    pub question_response: Option<crate::dialogue::QuestionResponse>,
    /// With request, confirm and retain PLAN without starting BUILD. Default continues to BUILD.
    #[serde(default)]
    pub plan_only: bool,
    /// Explicit user request to BUILD the saved plan_ready contract with this exact full digest.
    #[serde(default)]
    pub build_confirmed: Option<String>,
    /// Explicit implementation correction within existing acceptance, tests and paths.
    #[serde(default)]
    pub adjust_build: Option<crate::engine::AdjustBuildRequest>,
    /// Revalidate this run's existing draft and resume both Astra reviews without planning.
    #[serde(default)]
    pub recheck_pr: bool,
    /// Explicitly authorized execution without planning. Never set merely to avoid confirmation.
    #[serde(default)]
    pub build: Option<crate::engine::BuildRequest>,
    /// Absolute path to the repository Hwahap should work in.
    pub cwd: String,
    /// Stable identity of this parent Codex task. Reuse the same value on every step and run.
    pub host_session_id: String,
    /// The user's implementation request. Supply it only when starting a new run.
    #[serde(default)]
    pub request: Option<String>,
    /// The user's message, verbatim. Never paraphrase, complete, or invent it.
    #[serde(default)]
    pub user_input: Option<String>,
    /// Record the native child immediately after spawning it.
    #[serde(default)]
    pub registration: Option<NativeRegistration>,
    /// Relay an exact terminal native result; absent usage remains unknown.
    #[serde(default)]
    pub completion: Option<NativeCompletion>,
    /// Confirm an orphan and its commands have stopped before recovery.
    #[serde(default)]
    pub stopped: Option<NativeStopped>,
    /// Report a spawn error immediately; uncertain creation requires stop recovery.
    #[serde(default)]
    pub dispatch_failure: Option<NativeFailure>,
    /// Resume a confirmed no-child pause using newly observed host recovery evidence.
    #[serde(default)]
    pub resume: Option<NativeResume>,
}

/// Arguments to `hwahap_status`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct StatusArgs {
    #[serde(default)]
    pub include_cost_evidence: bool,
    /// Absolute path to the repository whose run should be reported.
    pub cwd: String,
}

/// Arguments to `hwahap_ship`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ShipArgs {
    /// Absolute path to the repository holding the finished run.
    pub cwd: String,
    /// The user's exact `SHIP <challenge>` line. Hwahap rejects anything else.
    pub confirmation: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct NativeBriefReference {
    /// Immutable request under the current run's .hwahap/artifacts directory.
    pub artifact: String,
    pub prompt_digest: String,
}

/// What every tool returns.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RunReport {
    /// Next page of the current planning frontier for the host's actual user-question UI.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub question_batch: Option<crate::dialogue::QuestionBatch>,
    /// The run's stable identifier.
    pub run_id: String,
    /// `plan`, `build`, or `review`.
    pub phase: String,
    /// The engine state, for diagnostics.
    pub state: String,
    /// `continue`, `repair_translation`, `await_user`, `completed`, `blocked`, or a `native_*` action.
    pub next: String,
    /// The text to show the user.
    pub message: String,
    /// The frozen plan's digest, once there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_digest: Option<String>,
    /// The draft pull request, once there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    /// Full brief on first offer; registered progress carries metadata and native_brief.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_brief: Option<NativeBriefReference>,
    /// Current native dispatch metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_dispatch: Option<NativeDispatch>,
    /// All retained native requests, including incomplete work; unknown usage is explicit.
    pub cost_evidence: Option<serde_json::Value>,
}

impl From<StepOutcome> for RunReport {
    fn from(outcome: StepOutcome) -> Self {
        RunReport {
            question_batch: None,
            run_id: outcome.run_id,
            phase: outcome.phase,
            state: outcome.state,
            next: outcome.next,
            message: outcome.message,
            plan_digest: outcome.plan_digest,
            pr_url: outcome.pr_url,
            native_dispatch: None,
            native_brief: None,
            cost_evidence: None,
        }
    }
}

fn default_wait_ms() -> u64 {
    30_000
}

impl RunReport {
    pub fn compact_native(&mut self) {
        if let Some(dispatch) = self.native_dispatch.as_mut() {
            self.native_brief = Some(NativeBriefReference {
                artifact: format!("native-request-{}.json", dispatch.dispatch_id),
                prompt_digest: dispatch.prompt_digest.clone(),
            });
            dispatch.brief.clear();
        }
    }
    fn attach_questions(&mut self, root: &std::path::Path) -> crate::Result<()> {
        if self.state == "deciding" && self.next == "await_user" {
            if let Some(plan) = crate::state::Store::open(root)?.read_plan()? {
                self.question_batch = crate::dialogue::QuestionBatch::derive(&plan)?;
            }
        }
        Ok(())
    }
}

impl From<NativeProgress> for RunReport {
    fn from(progress: NativeProgress) -> Self {
        let mut report = RunReport::from(progress.outcome);
        report.native_dispatch = progress.dispatch;
        if let Some(dispatch) = report.native_dispatch.as_mut() {
            if dispatch.agent_id.is_some() {
                report.native_brief = Some(NativeBriefReference {
                    artifact: format!("native-request-{}.json", dispatch.dispatch_id),
                    prompt_digest: dispatch.prompt_digest.clone(),
                });
                dispatch.brief.clear();
            }
        }
        report
    }
}

/// The MCP server.
///
/// NativeHost owns background continuations and repository locks. Tool requests return promptly;
/// status reads a snapshot while a native child or test command is running.
#[derive(Clone)]
pub struct Hwahap {
    tool_router: ToolRouter<Self>,
    native: std::sync::Arc<NativeHost>,
}

impl Default for Hwahap {
    fn default() -> Self {
        Self::new()
    }
}

// `vis = "pub"` because the generated constructor is private by default, which would put the
// "exactly three tools" gate out of reach of an integration test.
#[tool_router(router = tool_router, vis = "pub")]
impl Hwahap {
    pub async fn shutdown(&self) {
        self.native.shutdown().await;
    }

    pub fn new() -> Self {
        Hwahap {
            tool_router: Self::tool_router(),
            native: std::sync::Arc::new(NativeHost::default()),
        }
    }

    /// Start or advance the one active Hwahap run in this repository.
    #[tool(
        name = "hwahap_step",
        description = "Start or advance the Hwahap run for a repository. Supply `request` to \
                       start, `user_input` to pass the user's exact reply, and neither to let the \
                       run continue. Returns `next`, which tells you whether to call again \
                       immediately, wait for the user, or stop.",
        annotations(
            title = "Advance the Hwahap run",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn step(
        &self,
        Parameters(args): Parameters<StepArgs>,
    ) -> Result<Json<RunReport>, ErrorData> {
        let root = root_for(&args.cwd)?;
        let host_session_id = args.host_session_id.clone();
        let mut outcome = self
            .native
            .advance(
                &root,
                NativeInput {
                    host_observation: args.host_observation,
                    task_assessment: args.task_assessment,
                    verification_recovery: args.verification_recovery,
                    approved_plan: args.approved_plan,
                    question_response: args.question_response,
                    plan_only: args.plan_only,
                    build_confirmed: args.build_confirmed,
                    adjust_build: args.adjust_build,
                    recheck_pr: args.recheck_pr,
                    build: args.build,
                    host_session_id: Some(args.host_session_id),
                    request: args.request,
                    user_input: args.user_input,
                    registration: args.registration,
                    completion: args.completion,
                    stopped: args.stopped,
                    abandon: args.abandon,
                    dispatch_failure: args.dispatch_failure,
                    resume: args.resume,
                },
            )
            .await
            .map_err(to_error_data)?;
        if outcome.dispatch.is_none() && outcome.outcome.next == "native_wait" && args.wait_ms > 0 {
            self.native
                .wait_ready(&root, args.wait_ms.min(30_000))
                .await;
            outcome = self
                .native
                .advance(
                    &root,
                    NativeInput {
                        host_session_id: Some(host_session_id),
                        ..Default::default()
                    },
                )
                .await
                .map_err(to_error_data)?;
        }
        let store = crate::state::Store::open(&root).map_err(to_error_data)?;
        if let Some(path) = args.usage_session_path {
            crate::cost::meter::attach(&store, std::path::Path::new(&path), false)
                .map_err(to_error_data)?;
        }
        let mut report = RunReport::from(outcome);
        report.attach_questions(&root).map_err(to_error_data)?;
        report.cost_evidence = Some(crate::cost::for_report(
            crate::cost::persist(&store).map_err(to_error_data)?,
            args.include_cost_evidence,
        ));
        Ok(Json(report))
    }

    /// Report the run without changing it.
    #[tool(
        name = "hwahap_status",
        description = "Report the state of the Hwahap run in a repository without changing \
                       anything. Use this to answer 'how is it going?' during autonomous coding.",
        annotations(
            title = "Read the Hwahap run",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn status(
        &self,
        Parameters(args): Parameters<StatusArgs>,
    ) -> Result<Json<RunReport>, ErrorData> {
        let root = root_for(&args.cwd)?;
        let outcome = self.native.status(&root).await.map_err(to_error_data)?;
        let mut report = RunReport::from(outcome);
        report.attach_questions(&root).map_err(to_error_data)?;
        report.compact_native();
        report.cost_evidence = Some(crate::cost::for_report(
            crate::cost::summary(&crate::state::Store::open(&root).map_err(to_error_data)?)
                .map_err(to_error_data)?,
            args.include_cost_evidence,
        ));
        Ok(Json(report))
    }

    /// Mark the finished draft pull request ready for review.
    #[tool(
        name = "hwahap_ship",
        description = "Mark the finished draft pull request ready for review. Call this only after \
                       the user has typed an exact `SHIP <challenge>` line themselves; pass that \
                       line verbatim as `confirmation`. Hwahap does not merge and does not enable \
                       auto-merge.",
        annotations(
            title = "Ship the draft pull request",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn ship(
        &self,
        Parameters(args): Parameters<ShipArgs>,
    ) -> Result<Json<RunReport>, ErrorData> {
        let root = root_for(&args.cwd)?;
        let outcome = self
            .native
            .ship(&root, &args.confirmation)
            .await
            .map_err(to_error_data)?;
        let mut report = RunReport::from(outcome);
        report.cost_evidence = Some(
            crate::cost::persist(&crate::state::Store::open(&root).map_err(to_error_data)?)
                .map_err(to_error_data)?,
        );
        Ok(Json(report))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Hwahap {
    fn get_info(&self) -> ServerInfo {
        // Written by hand rather than synthesized by the macro: without an explicit name the
        // generated version identifies the server as "rmcp", because the `env!` calls expand
        // inside the rmcp crate.
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("hwahap", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}

#[cfg(test)]
fn engine_for(cwd: &str) -> Result<crate::engine::Engine, ErrorData> {
    crate::engine::Engine::open(&root_for(cwd)?).map_err(to_error_data)
}

fn root_for(cwd: &str) -> Result<PathBuf, ErrorData> {
    let path = PathBuf::from(cwd);
    if !path.is_absolute() {
        return Err(ErrorData::invalid_params(
            format!("cwd must be an absolute path, got {cwd:?}"),
            None,
        ));
    }
    Git::open(&path)
        .map(|git| git.root().to_path_buf())
        .map_err(to_error_data)
}

/// Maps a Hwahap error onto the MCP error the host will render.
///
/// Bad arguments are protocol errors; everything else is a run-level failure the user needs to
/// read, so it keeps its own message.
fn to_error_data(error: Error) -> ErrorData {
    match error {
        Error::Rejected(message) => ErrorData::invalid_params(message, None),
        other => ErrorData::internal_error(other.to_string(), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_are_exactly_three_tools() {
        let tools = Hwahap::tool_router().list_all();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        assert_eq!(names, vec!["hwahap_ship", "hwahap_status", "hwahap_step"]);
    }

    #[test]
    fn exactly_one_tool_is_read_only() {
        let read_only: Vec<String> = Hwahap::tool_router()
            .list_all()
            .iter()
            .filter(|t| {
                t.annotations
                    .as_ref()
                    .and_then(|a| a.read_only_hint)
                    .unwrap_or(false)
            })
            .map(|t| t.name.to_string())
            .collect();
        assert_eq!(read_only, vec!["hwahap_status".to_string()]);
    }

    #[test]
    fn no_tool_is_marked_destructive() {
        // Hwahap creates a branch and a draft PR; it never deletes or merges. A destructive hint
        // would ask the host to gate work that is in fact reversible.
        for tool in Hwahap::tool_router().list_all() {
            let destructive = tool.annotations.as_ref().and_then(|a| a.destructive_hint);
            assert_eq!(
                destructive,
                Some(false),
                "{} claims to be destructive",
                tool.name
            );
        }
    }

    #[test]
    fn every_tool_has_a_description_and_a_title() {
        for tool in Hwahap::tool_router().list_all() {
            let description = tool.description.as_deref().unwrap_or_default();
            assert!(
                description.len() > 40,
                "{} has a thin description",
                tool.name
            );
            assert!(
                tool.annotations
                    .as_ref()
                    .and_then(|a| a.title.as_deref())
                    .is_some(),
                "{} has no title",
                tool.name
            );
        }
    }

    #[test]
    fn tool_descriptions_do_not_overlap() {
        // Two tools whose descriptions could each answer the same question make the host guess.
        let tools = Hwahap::tool_router().list_all();
        for a in &tools {
            for b in &tools {
                if a.name >= b.name {
                    continue;
                }
                assert_ne!(
                    a.description, b.description,
                    "{} and {} share a description",
                    a.name, b.name
                );
            }
        }
    }

    #[test]
    fn the_server_identifies_itself_and_not_rmcp() {
        let info = Hwahap::new().get_info();
        assert_eq!(info.server_info.name, "hwahap");
        assert_eq!(info.server_info.version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn the_instructions_are_advertised_and_self_contained_at_the_front() {
        let info = Hwahap::new().get_info();
        let instructions = info.instructions.expect("instructions must be advertised");
        assert_eq!(instructions, INSTRUCTIONS);

        let opening: String = instructions.chars().take(512).collect();
        for expected in [
            "hwahap_step",
            "continue",
            "await_user",
            "CONFIRM PLAN",
            "SHIP",
        ] {
            assert!(
                opening.contains(expected),
                "the first 512 chars omit {expected:?}"
            );
        }
    }

    #[test]
    fn the_instructions_name_every_tool() {
        for tool in Hwahap::tool_router().list_all() {
            assert!(
                INSTRUCTIONS.contains(tool.name.as_ref()),
                "the instructions never mention {}",
                tool.name
            );
        }
    }

    #[test]
    fn the_instructions_forbid_the_host_from_inventing_a_confirmation() {
        assert!(INSTRUCTIONS.contains("Never compose, complete, or infer"));
        assert!(INSTRUCTIONS.contains("only the user may type one"));
    }

    #[test]
    fn a_relative_cwd_is_rejected_as_a_parameter_error() {
        let Err(err) = engine_for("relative/path") else {
            panic!("a relative cwd must be rejected");
        };
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert!(err.message.contains("absolute"), "{}", err.message);
    }

    #[test]
    fn a_rejection_becomes_invalid_params_and_anything_else_becomes_internal_error() {
        assert_eq!(
            to_error_data(Error::Rejected("no".into())).code,
            rmcp::model::ErrorCode::INVALID_PARAMS
        );
        assert_eq!(
            to_error_data(Error::UnsupportedProfile("x".into())).code,
            rmcp::model::ErrorCode::INTERNAL_ERROR
        );
        assert!(to_error_data(Error::UnsupportedProfile("x".into()))
            .message
            .contains("unsupported_profile"));
    }

    #[test]
    fn the_step_tool_requires_repository_and_parent_identity() {
        let tool = Hwahap::step_tool_attr();
        let schema = serde_json::to_value(&tool.input_schema).unwrap();
        let required = schema
            .get("required")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();
        let required: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(required, vec!["cwd", "host_session_id"]);
        let properties = schema
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(properties.contains_key("request"));
        assert!(properties.contains_key("user_input"));
    }

    #[test]
    fn the_ship_tool_requires_the_users_confirmation_line() {
        let tool = Hwahap::ship_tool_attr();
        let schema = serde_json::to_value(&tool.input_schema).unwrap();
        let required: Vec<String> = schema
            .get("required")
            .and_then(|r| r.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            required.contains(&"confirmation".to_string()),
            "{required:?}"
        );
        assert!(required.contains(&"cwd".to_string()), "{required:?}");
    }
}
