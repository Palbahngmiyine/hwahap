//! The strict JSON contract a worker or reviewer session must end with.
//!
//! There is no MCP server facing the workers: a worker reports by making its *final* message one
//! JSON object and nothing else. That keeps the process budget fixed, and it keeps the control
//! channel narrow enough to validate exactly.
//!
//! What arrives here is control metadata only. "The tests passed" is never believed from this JSON;
//! the host runs the command and reads the exit status, and it reads the git diff for the changed
//! paths. This module's whole job is to decide whether the agent said something well-formed, and to
//! say precisely what was wrong when it did not, so the rework prompt can quote the violation.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// The worker's own account of what it did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerResult {
    pub status: WorkerStatus,
    pub summary: String,
    /// Set only for [`WorkerStatus::PlanConflict`]; the frozen plan detail that cannot hold.
    #[serde(default)]
    pub conflict: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerStatus {
    Completed,
    /// The unit cannot be built without making a product decision the frozen plan does not contain.
    PlanConflict,
    Failed,
}

/// The reviewer's verdict on a diff it may only read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewResult {
    pub verdict: Verdict,
    #[serde(default)]
    pub findings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Fail,
}

impl WorkerResult {
    /// The exact shape a worker is told to emit, quoted back to it on a violation.
    pub const CONTRACT: &'static str = r#"{"status":"completed|plan_conflict|failed","summary":"<one paragraph>","conflict":null}"#;

    /// Parses a worker's final message.
    pub fn parse(final_message: &str) -> Result<WorkerResult> {
        let result: WorkerResult = parse_strict(final_message, Self::CONTRACT)?;
        // Trimmed, because a run of spaces names no plan detail either. The conflict is the one
        // thing the user is shown as the reason the frozen plan stopped, and the engine falls back
        // to the summary only when the field is absent — a blank string would defeat that fallback
        // and stop the run with an empty reason.
        match (result.status, result.conflict.as_deref().map(str::trim)) {
            (WorkerStatus::PlanConflict, None | Some("")) => Err(violation(
                "status is plan_conflict but conflict is empty; name the plan detail that cannot hold",
                Self::CONTRACT,
            )),
            (WorkerStatus::Completed | WorkerStatus::Failed, Some(detail)) if !detail.is_empty() => {
                Err(violation(
                    "conflict is set but status is not plan_conflict",
                    Self::CONTRACT,
                ))
            }
            _ if result.summary.trim().is_empty() => {
                Err(violation("summary is empty", Self::CONTRACT))
            }
            _ => Ok(result),
        }
    }
}

impl ReviewResult {
    /// The exact shape a reviewer is told to emit.
    pub const CONTRACT: &'static str = r#"{"verdict":"pass|fail","findings":["<finding>"]}"#;

    /// Parses a reviewer's final message.
    pub fn parse(final_message: &str) -> Result<ReviewResult> {
        let mut result: ReviewResult = parse_strict(final_message, Self::CONTRACT)?;
        // Checked before the blanks are dropped: a pass that carries anything at all in `findings`
        // is a reviewer contradicting itself, whatever the entries hold.
        if result.verdict == Verdict::Pass && !result.findings.is_empty() {
            return Err(violation(
                "verdict is pass with findings; raise them as fail or drop them",
                Self::CONTRACT,
            ));
        }
        // A blank finding is not a finding: it would reach the rework prompt as an empty bullet the
        // worker cannot act on, and it would hide a rejection that named nothing at all.
        result.findings.retain(|f| !f.trim().is_empty());
        if result.verdict == Verdict::Fail && result.findings.is_empty() {
            return Err(violation(
                "verdict is fail with no findings; a rejection the worker cannot act on is not a review",
                Self::CONTRACT,
            ));
        }
        Ok(result)
    }
}

/// Parses a final message that must be one bare JSON object and nothing else.
///
/// Deliberately unforgiving: no surrounding prose, no ```json fence, no second object. A parser
/// that digs a JSON object out of prose will eventually dig the wrong one out, and the cost of
/// strictness is one rework round that quotes the exact contract back.
pub(crate) fn parse_strict<T: serde::de::DeserializeOwned>(
    final_message: &str,
    contract: &str,
) -> Result<T> {
    let text = final_message.trim();
    if text.is_empty() {
        return Err(violation("the final message was empty", contract));
    }
    if text.starts_with("```") {
        return Err(violation(
            "the final message was wrapped in a code fence; emit the bare JSON object",
            contract,
        ));
    }
    if !text.starts_with('{') {
        return Err(violation(
            "the final message did not start with a JSON object",
            contract,
        ));
    }
    serde_json::from_str(text).map_err(|e| violation(&e.to_string(), contract))
}

fn violation(detail: &str, contract: &str) -> Error {
    Error::Rejected(format!(
        "the agent's final message did not satisfy the result contract: {detail}. \
         The final message must be exactly this JSON object and nothing else: {contract}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message_of(err: Error) -> String {
        err.to_string()
    }

    #[test]
    fn a_completed_worker_result_parses() {
        let result =
            WorkerResult::parse(r#"{"status":"completed","summary":"added the flag"}"#).unwrap();
        assert_eq!(result.status, WorkerStatus::Completed);
        assert_eq!(result.summary, "added the flag");
        assert_eq!(result.conflict, None);
    }

    #[test]
    fn surrounding_whitespace_and_newlines_are_tolerated() {
        let result =
            WorkerResult::parse("\n\n  {\"status\":\"failed\",\"summary\":\"tests fail\"}  \n")
                .unwrap();
        assert_eq!(result.status, WorkerStatus::Failed);
    }

    #[test]
    fn an_explicit_null_conflict_is_accepted_for_a_completed_result() {
        let result =
            WorkerResult::parse(r#"{"status":"completed","summary":"done","conflict":null}"#)
                .unwrap();
        assert_eq!(result.conflict, None);
    }

    #[test]
    fn a_plan_conflict_must_name_the_conflict() {
        for message in [
            r#"{"status":"plan_conflict","summary":"cannot build"}"#,
            r#"{"status":"plan_conflict","summary":"cannot build","conflict":null}"#,
            r#"{"status":"plan_conflict","summary":"cannot build","conflict":""}"#,
        ] {
            let err = message_of(WorkerResult::parse(message).unwrap_err());
            assert!(err.contains("conflict is empty"), "{err}");
            assert!(err.contains("plan_conflict"), "{err}");
        }
    }

    #[test]
    fn a_plan_conflict_whose_conflict_is_only_whitespace_is_rejected() {
        for conflict in ["   ", "\n", "\t \n"] {
            let message = serde_json::json!({
                "status": "plan_conflict", "summary": "cannot build", "conflict": conflict
            })
            .to_string();
            let err = message_of(WorkerResult::parse(&message).unwrap_err());
            assert!(err.contains("conflict is empty"), "{conflict:?} -> {err}");
        }
    }

    #[test]
    fn a_blank_finding_never_reaches_the_rework_prompt() {
        let result =
            ReviewResult::parse(r#"{"verdict":"fail","findings":["","  ","U3 has no test"]}"#)
                .unwrap();
        assert_eq!(result.findings, vec!["U3 has no test"]);
    }

    #[test]
    fn a_plan_conflict_with_detail_parses() {
        let result = WorkerResult::parse(
            r#"{"status":"plan_conflict","summary":"webhook is async","conflict":"C1 assumes a sync webhook"}"#,
        )
        .unwrap();
        assert_eq!(result.status, WorkerStatus::PlanConflict);
        assert_eq!(
            result.conflict.as_deref(),
            Some("C1 assumes a sync webhook")
        );
    }

    #[test]
    fn a_conflict_on_a_non_conflict_status_is_rejected() {
        for status in ["completed", "failed"] {
            let message =
                format!(r#"{{"status":"{status}","summary":"s","conflict":"something"}}"#);
            let err = message_of(WorkerResult::parse(&message).unwrap_err());
            assert!(
                err.contains("conflict is set but status is not plan_conflict"),
                "{err}"
            );
        }
    }

    #[test]
    fn an_empty_summary_is_rejected() {
        for summary in ["", "   ", "\n\t"] {
            let message =
                serde_json::json!({"status": "completed", "summary": summary}).to_string();
            let err = message_of(WorkerResult::parse(&message).unwrap_err());
            assert!(err.contains("summary is empty"), "{err}");
        }
    }

    #[test]
    fn prose_around_the_object_is_rejected() {
        for message in [
            r#"Here is my result: {"status":"completed","summary":"s"}"#,
            r#"{"status":"completed","summary":"s"} — let me know if you need more."#,
            r#"{"status":"completed","summary":"s"}{"status":"failed","summary":"s"}"#,
        ] {
            assert!(
                WorkerResult::parse(message).is_err(),
                "should have rejected {message:?}"
            );
        }
    }

    #[test]
    fn a_code_fence_is_rejected_with_a_specific_message() {
        let err = message_of(
            WorkerResult::parse("```json\n{\"status\":\"completed\",\"summary\":\"s\"}\n```")
                .unwrap_err(),
        );
        assert!(err.contains("code fence"), "{err}");
        assert!(err.contains(WorkerResult::CONTRACT), "{err}");
    }

    #[test]
    fn an_empty_final_message_is_rejected() {
        for message in ["", "   \n  "] {
            let err = message_of(WorkerResult::parse(message).unwrap_err());
            assert!(err.contains("was empty"), "{err}");
        }
    }

    #[test]
    fn a_json_array_or_scalar_is_rejected() {
        for message in [
            "[]",
            r#"["completed"]"#,
            "\"completed\"",
            "42",
            "null",
            "true",
        ] {
            let err = message_of(WorkerResult::parse(message).unwrap_err());
            assert!(
                err.contains("did not start with a JSON object"),
                "{message} -> {err}"
            );
        }
    }

    #[test]
    fn an_unknown_field_is_rejected_rather_than_ignored() {
        let err = message_of(
            WorkerResult::parse(r#"{"status":"completed","summary":"s","tests_passed":true}"#)
                .unwrap_err(),
        );
        assert!(err.contains("tests_passed"), "{err}");
    }

    #[test]
    fn an_unknown_status_is_rejected() {
        for status in [
            "ok",
            "success",
            "Completed",
            "COMPLETED",
            "plan-conflict",
            "",
        ] {
            let message = serde_json::json!({"status": status, "summary": "s"}).to_string();
            assert!(
                WorkerResult::parse(&message).is_err(),
                "should have rejected {status:?}"
            );
        }
    }

    #[test]
    fn a_missing_required_field_is_rejected() {
        for message in [r#"{"summary":"s"}"#, r#"{"status":"completed"}"#, "{}"] {
            assert!(
                WorkerResult::parse(message).is_err(),
                "should have rejected {message}"
            );
        }
    }

    #[test]
    fn a_summary_with_unicode_and_newlines_survives_verbatim() {
        let summary = "구현 완료 ✅\n- src/apply/mod.rs 수정\ncafe\u{0301}";
        let message = serde_json::json!({"status": "completed", "summary": summary}).to_string();
        assert_eq!(WorkerResult::parse(&message).unwrap().summary, summary);
    }

    #[test]
    fn a_passing_review_parses_and_must_carry_no_findings() {
        let result = ReviewResult::parse(r#"{"verdict":"pass","findings":[]}"#).unwrap();
        assert_eq!(result.verdict, Verdict::Pass);
        assert!(result.findings.is_empty());

        let result = ReviewResult::parse(r#"{"verdict":"pass"}"#).unwrap();
        assert!(result.findings.is_empty());

        let err = message_of(
            ReviewResult::parse(r#"{"verdict":"pass","findings":["nit: naming"]}"#).unwrap_err(),
        );
        assert!(err.contains("pass with findings"), "{err}");
    }

    #[test]
    fn a_failing_review_must_say_why() {
        for message in [
            r#"{"verdict":"fail","findings":[]}"#,
            r#"{"verdict":"fail"}"#,
            r#"{"verdict":"fail","findings":["  "]}"#,
        ] {
            let err = message_of(ReviewResult::parse(message).unwrap_err());
            assert!(err.contains("no findings"), "{message} -> {err}");
        }
    }

    #[test]
    fn a_failing_review_with_findings_parses() {
        let result =
            ReviewResult::parse(r#"{"verdict":"fail","findings":["U3 changes an unlisted path"]}"#)
                .unwrap();
        assert_eq!(result.verdict, Verdict::Fail);
        assert_eq!(result.findings, vec!["U3 changes an unlisted path"]);
    }

    #[test]
    fn an_unknown_verdict_is_rejected() {
        for verdict in ["passed", "PASS", "approve", "ok", ""] {
            let message = serde_json::json!({"verdict": verdict}).to_string();
            assert!(
                ReviewResult::parse(&message).is_err(),
                "should have rejected {verdict:?}"
            );
        }
    }

    #[test]
    fn every_violation_quotes_the_contract_back() {
        let worker = message_of(WorkerResult::parse("not json").unwrap_err());
        assert!(worker.contains(WorkerResult::CONTRACT), "{worker}");
        let review = message_of(ReviewResult::parse("not json").unwrap_err());
        assert!(review.contains(ReviewResult::CONTRACT), "{review}");
    }

    #[test]
    fn results_round_trip_through_json() {
        let worker = WorkerResult {
            status: WorkerStatus::PlanConflict,
            summary: "s".into(),
            conflict: Some("c".into()),
        };
        let encoded = serde_json::to_string(&worker).unwrap();
        assert_eq!(WorkerResult::parse(&encoded).unwrap(), worker);

        let review = ReviewResult {
            verdict: Verdict::Fail,
            findings: vec!["f".into()],
        };
        let encoded = serde_json::to_string(&review).unwrap();
        assert_eq!(ReviewResult::parse(&encoded).unwrap(), review);
    }

    #[test]
    fn the_contracts_are_themselves_valid_json_templates() {
        // A contract string that is not parseable JSON would be quoted back as guidance the agent
        // cannot follow, so keep it structurally valid apart from its enum placeholders.
        for contract in [WorkerResult::CONTRACT, ReviewResult::CONTRACT] {
            let value: serde_json::Value = serde_json::from_str(contract).unwrap();
            assert!(value.is_object(), "{contract}");
        }
    }
}
