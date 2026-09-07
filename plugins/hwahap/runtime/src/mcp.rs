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
pub const INSTRUCTIONS: &str = r#"Hwahap supplies scoped execution, verification evidence, risk-based delegation and recovery. The host owns its Plan/Goal UI, task lifecycle and model tools. Use hwahap_step with one action, the absolute repository path and a stable host_session_id. Follow next: continue advances; native_dispatch offers work; native_wait awaits work; await_checks awaits CI; await_user asks for the missing decision; blocked/delegation_wait/native_paused reports the cause and awaits new evidence. completed ends the run. Relay CONFIRM PLAN and SHIP verbatim from the user. hwahap_status reads progress; hwahap_ship marks a verified draft ready.

Reuse the host's approved Plan via approved_plan: original plan, source HEAD, implementation request and bound approval reference. Missing plan.json is a handoff to recover. Preserve approval during translation repair and ask only for new material choices. Use plan_only:true for planning alone, build_confirmed for an approved saved plan, and build only for explicit execution without planning. adjust_build corrects implementation within the frozen contract; changed scope returns to planning. Optional task_profiles bind requirements, topology and risk in both planned and imported contracts.

The host retains its existing Goal. Optional host_context carries provider, task_id, goal_ref, plan_ref and callable capabilities. Treat these references as metadata, not authority. The host creates Goals only on explicit request, preserves user budgets and controls pause/resume. Compare the entire Goal completion criteria with run evidence before marking it complete. One accepted unit or run may leave Goal work. Use native Plan, question and Goal tools only when actually available; otherwise pass approved text and expose the unsupported capability.

For question_batch use a callable question tool in the current mode: one short question, alternatives in options, supporting evidence in links. Relay actual answers as question_response with the exact batch ID and answer text. Defaults, cancellation and timeout remain unanswered. Forward user_input verbatim. Never compose, complete, or infer CONFIRM PLAN or SHIP lines; only the user may type one. A bound existing approval enters BUILD without another confirmation.

Dispatch: send the exact brief once, obey cwd/access, role, selected model/effort and decision digest. For coordinator register agent_id=coordinator and work in the qualified parent. For reuse_agent_id register that identity before one follow-up. Otherwise create one child with the requested model/effort and no inherited history, then register its returned identity immediately. Workers perform only assigned work; reviewers remain independent of authors. Model/catalog changes apply to new runs. Preserve bound identities and settings within a run; unavailable models enter recovery.

Completion: stop the worker's turn and commands, then relay its exact dispatch_id/result JSON envelope with agent_stopped:true and matching decision_digest. Keep reported_usage null unless real counters exist. Registration/completion retries retain their original identity and payload. On ambiguous delivery, recover rather than deliver again. dispatch_failure preserves the exact host error; no_agent_created is true only if no child exists and no follow-up was attempted. native_stop requires verified termination of that worker and all its commands before stopped acknowledgment. Missing identities require explicit recovery or abandon with preserved evidence and a successor run; never fabricate a review or silently substitute identity.

Waiting: hwahap_step waits internally up to 30 seconds for engine progress (wait_ms:0 returns immediately). For a registered worker, await the host's native completion event. For CI, await the host/forge check event. Avoid model-driven polling. Repeat reports carry native_brief references; read the immutable request artifact when its brief is needed. Worktree paths come from the dispatch, so reuse the host's workspace only when the execution contract supports it.

Cost: minimize repeated full suites, full-history handoffs and unchanged status calls. Run focused author checks; the engine runs acceptance checks. Supply usage_session_path when a host log is available: attachment covers its baseline onward, and unavailable optional metering is reported separately from execution success. include_cost_evidence:true returns full details; ordinary progress returns a bounded summary and .hwahap/usage.json. Session and dispatch totals overlap; keep unknown usage and actual billing separate. Verification reuse is opt-in with an environment_revision and complete declared inputs; preflight recovery always executes. Report savings only against equal completion criteria including retries and quality.

PR: CI failure becomes a bound repair obligation before model review. A complete frozen low-risk profile permits one clean independent PR review; findings, uncertain risk and repair work retain attack/defense. Use recheck_pr:true alone for the current draft, preserving retry budgets and eligible verification evidence. Author checkpoints must respect the repository's rules and the assigned HEAD ownership; arrange compatible checkpoint refs before writing. Resume an interrupted run only with new observed recovery evidence. Archive/abandon preserves code and evidence for a successor; it does not imply successful completion. All edits, tests and publication follow the user's scope and current dispatch authorization."#;

/// Arguments to `hwahap_step`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct StepArgs {
    /// Optional Plan/Goal references and callable features from the host.
    #[serde(default)]
    pub host_context: Option<crate::host_context::HostContext>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_context: Option<serde_json::Value>,
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
            host_context: None,
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
        if let Some(context) = &args.host_context {
            context
                .validate(&args.host_session_id)
                .map_err(to_error_data)?;
        }
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
        let usage_attachment = args.usage_session_path.map(|path| {
            match crate::cost::meter::attach(&store, std::path::Path::new(&path), false) {
                Ok(session) => serde_json::json!({"status":"attached","session_id":session,"coverage":"from attachment baseline"}),
                Err(error) => serde_json::json!({"status":"unavailable","error":error.to_string()}),
            }
        });
        let context_result = args
            .host_context
            .as_ref()
            .map(|context| crate::host_context::record(&store, context));
        let mut report = RunReport::from(outcome);
        report.host_context = match context_result {
            Some(Err(error)) => {
                Some(serde_json::json!({"status":"unavailable","error":error.to_string()}))
            }
            _ => crate::host_context::report(&store).unwrap_or_else(|error| {
                Some(serde_json::json!({"status":"unavailable","error":error.to_string()}))
            }),
        };
        report.attach_questions(&root).map_err(to_error_data)?;
        report.cost_evidence = Some(crate::cost::for_report(
            crate::cost::persist(&store).map_err(to_error_data)?,
            args.include_cost_evidence,
        ));
        if let Some(attachment) = usage_attachment {
            report.cost_evidence.as_mut().expect("cost report")["usage_attachment"] = attachment;
        }
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
        report.host_context =
            crate::host_context::report(&crate::state::Store::open(&root).map_err(to_error_data)?)
                .map_err(to_error_data)?;
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

    #[cfg(unix)]
    #[tokio::test]
    async fn invalid_optional_usage_keeps_the_committed_action_visible() {
        let dir = tempfile::tempdir().unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec![
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "--allow-empty",
                "-m",
                "seed",
            ],
        ] {
            assert!(std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .unwrap()
                .status
                .success());
        }
        std::fs::write(dir.path().join(".git/info/exclude"), "/.hwahap/\n").unwrap();
        let server = Hwahap::new();
        let args = serde_json::from_value(serde_json::json!({"cwd":dir.path(),"host_session_id":"fixture","request":"Inspect the empty repository","plan_only":true,"usage_session_path":dir.path().join("missing.jsonl"),"host_context":{"provider":"codex","task_id":"fixture","goal_ref":"goal:fixture"}})).unwrap();
        let Json(report) = server.step(Parameters(args)).await.unwrap();
        assert!(!report.run_id.is_empty());
        assert_eq!(
            report.host_context.as_ref().unwrap()["references"]["goal_ref"],
            "goal:fixture"
        );
        assert_eq!(
            report.cost_evidence.unwrap()["usage_attachment"]["status"],
            "unavailable"
        );
        server.native.shutdown().await;
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
