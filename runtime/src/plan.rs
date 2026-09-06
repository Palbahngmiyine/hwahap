//! The `hwahap/v4` plan contract.
//!
//! The plan is the only thing the coding engine is allowed to act on. Everything the user decided
//! lives here, and nothing else does: there is no separate answers database, no side table of
//! defaults, and no place for an agent to record a decision the user never made.
//!
//! Two digests per decision drive answer freshness:
//! - [`Decision::identity_digest`] covers the question and its alternatives. Any answer is bound to
//!   it, so rewording a question invalidates the answer to it.
//! - [`Decision::recommendation_digest`] covers the recommendation. Only `C<n>=REC` answers are
//!   bound to it, because "I want whatever you recommend" is an answer *about the recommendation*,
//!   while `C<n>=ALT2` is an answer about the alternative and survives a changed recommendation.

use serde::{Deserialize, Serialize};

use crate::canonical::Digest;
use crate::error::{Error, Result};

/// The schema tag written into, and required from, `plan.json`.
pub const SCHEMA: &str = "hwahap/v4";

/// The twelve decision surfaces. They are a checklist, never a stage.
pub const SURFACES: [Surface; 12] = [
    Surface::S1,
    Surface::S2,
    Surface::S3,
    Surface::S4,
    Surface::S5,
    Surface::S6,
    Surface::S7,
    Surface::S8,
    Surface::S9,
    Surface::S10,
    Surface::S11,
    Surface::S12,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Surface {
    S1,
    S2,
    S3,
    S4,
    S5,
    S6,
    S7,
    S8,
    S9,
    S10,
    S11,
    S12,
}

impl Surface {
    /// The surface's user-facing title, rendered into `plan.md`.
    pub fn title(self) -> &'static str {
        match self {
            Surface::S1 => "goal, success, failure, non-goals, scope",
            Surface::S2 => "user action, defaults, ordering, atomicity, idempotency",
            Surface::S3 => "errors, partial failure, recovery, rollback, retry",
            Surface::S4 => "commands, APIs, events, configuration, types, paths, formats",
            Surface::S5 => "data and state ownership, lifetime, persistence, deletion",
            Surface::S6 => "API, file, and internal contracts, schema, versioning",
            Surface::S7 => "architecture, module boundaries, dependencies",
            Surface::S8 => "concurrency, timing, timeouts, ordering, resource limits",
            Surface::S9 => "compatibility, migration, rollout, downgrade",
            Surface::S10 => "security, privacy, authorization, secrets, side effects",
            Surface::S11 => "performance, observability, operations",
            Surface::S12 => "verification setup, input, action, observable, evidence",
        }
    }

    /// Parses `S1`..`S12`.
    pub fn parse(text: &str) -> Option<Surface> {
        SURFACES.into_iter().find(|s| s.id() == text)
    }

    /// The `S<n>` identifier.
    pub fn id(self) -> &'static str {
        match self {
            Surface::S1 => "S1",
            Surface::S2 => "S2",
            Surface::S3 => "S3",
            Surface::S4 => "S4",
            Surface::S5 => "S5",
            Surface::S6 => "S6",
            Surface::S7 => "S7",
            Surface::S8 => "S8",
            Surface::S9 => "S9",
            Surface::S10 => "S10",
            Surface::S11 => "S11",
            Surface::S12 => "S12",
        }
    }
}

impl std::fmt::Display for Surface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id())
    }
}

/// Whether a surface applies to this goal.
///
/// `NotApplicable` carries the user's own `S<n>=NA`; Hwahap proposes the reason but may not close a
/// surface on its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SurfaceStatus {
    Applicable,
    NotApplicable { reason: String, answer: Answer },
}

/// A repository fact Hwahap established instead of asking the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fact {
    /// `F<n>`.
    pub id: String,
    pub question: String,
    pub answer: String,
    /// Source locations, e.g. `src/apply/mod.rs:120-146`.
    pub sources: Vec<String>,
}

/// One option the user may pick.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alternative {
    /// `ALT<n>`.
    pub id: String,
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

/// What Hwahap advises, and why.
///
/// A recommendation is never an implicit default: it is displayed, and the user must still type
/// `C<n>=REC` for it to take effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum Recommendation {
    /// Hwahap advises `choice`.
    Recommended {
        /// The `ALT<n>` id being recommended.
        choice: String,
        rationale: Vec<String>,
        /// `F<n>` fact ids supporting the rationale.
        evidence: Vec<String>,
        tradeoffs: Vec<String>,
        /// What the choice changes, e.g. `api`, `tests`.
        impact: Vec<String>,
        confidence: Confidence,
    },
    /// There is no objective basis to prefer an alternative; the user must choose one outright.
    NoRecommendation { rationale: Vec<String> },
    /// The answer needs an experiment first; `probe_unit` runs it.
    ProbeRequired {
        /// The `U<n>` probe unit that resolves this decision.
        probe_unit: String,
        rationale: Vec<String>,
    },
}

impl Recommendation {
    /// The recommended `ALT<n>`, if this recommendation names one.
    pub fn recommended_alternative(&self) -> Option<&str> {
        match self {
            Recommendation::Recommended { choice, .. } => Some(choice),
            _ => None,
        }
    }
}

/// What kind of question a decision asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    /// A product or technical choice.
    Decision,
    /// "What must happen when …".
    Scenario,
    /// Which word the plan and the code will use.
    Term,
}

/// What the user chose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Selection {
    /// `C<n>=REC`: take the recommendation as displayed.
    Recommendation,
    /// `C<n>=ALT<m>`.
    Alternative { id: String },
    /// `C<n>=OTHER: <value>`.
    Other { value: String },
    /// `C<n>=UNKNOWN`: becomes an open item, resolved by a fact or a probe unit.
    Unknown,
    /// `S<n>=NA`.
    NotApplicable,
}

/// A recorded user answer, bound to the exact text it answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    /// The user's message line, verbatim.
    pub text: String,
    pub selection: Selection,
    /// RFC 3339 UTC timestamp.
    pub ts: String,
    /// Digest of the question and its alternatives at the time of answering.
    pub identity: Digest,
    /// Digest of the recommendation, recorded only for `C<n>=REC`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommendation: Option<Digest>,
}

/// One material decision put to the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    /// `C<n>`.
    pub id: String,
    pub surface: Surface,
    pub kind: DecisionKind,
    pub question: String,
    pub alternatives: Vec<Alternative>,
    pub recommendation: Recommendation,
    /// `C<n>` ids that must be answered before this question can be asked.
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<Answer>,
}

impl Decision {
    /// Digest of everything an answer of any kind depends on.
    ///
    /// Deliberately excludes the recommendation: re-advising does not unmake an explicit choice.
    pub fn identity_digest(&self) -> Result<Digest> {
        Digest::of(&serde_json::json!({
            "id": self.id,
            "surface": self.surface,
            "kind": self.kind,
            "question": self.question,
            "alternatives": self.alternatives,
        }))
    }

    /// Digest of the recommendation, which `C<n>=REC` additionally depends on.
    pub fn recommendation_digest(&self) -> Result<Digest> {
        Digest::of(&self.recommendation)
    }

    /// Whether this decision currently counts as answered.
    ///
    /// A stale answer does not count: the question moved out from under it, so the user has not
    /// actually answered the question now being asked.
    pub fn is_answered(&self) -> Result<bool> {
        Ok(matches!(self.answer_freshness()?, AnswerFreshness::Fresh))
    }

    /// Why this decision's answer does or does not still count.
    pub fn answer_freshness(&self) -> Result<AnswerFreshness> {
        let Some(answer) = &self.answer else {
            return Ok(AnswerFreshness::Missing);
        };
        if answer.identity != self.identity_digest()? {
            return Ok(AnswerFreshness::StaleQuestion);
        }
        if matches!(answer.selection, Selection::Recommendation) {
            match &answer.recommendation {
                None => return Ok(AnswerFreshness::StaleRecommendation),
                Some(recorded) if *recorded != self.recommendation_digest()? => {
                    return Ok(AnswerFreshness::StaleRecommendation)
                }
                Some(_) => {}
            }
        }
        Ok(AnswerFreshness::Fresh)
    }

    /// The alternative the plan will build, once answered.
    ///
    /// `Unknown` yields `None`: it is an open item, not a resolution.
    pub fn resolved_value(&self) -> Result<Option<String>> {
        if !self.is_answered()? {
            return Ok(None);
        }
        let answer = self.answer.as_ref().expect("a fresh answer exists");
        Ok(match &answer.selection {
            Selection::Recommendation => self
                .recommendation
                .recommended_alternative()
                .and_then(|id| self.alternatives.iter().find(|a| a.id == id))
                .map(|a| a.value.clone()),
            Selection::Alternative { id } => self
                .alternatives
                .iter()
                .find(|a| &a.id == id)
                .map(|a| a.value.clone()),
            Selection::Other { value } => Some(value.clone()),
            Selection::Unknown | Selection::NotApplicable => None,
        })
    }
}

/// Why a decision's recorded answer does or does not still count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerFreshness {
    Fresh,
    /// No answer was ever recorded.
    Missing,
    /// The question or its alternatives changed after the answer.
    StaleQuestion,
    /// A `C<n>=REC` answer whose recommendation has since changed.
    StaleRecommendation,
}

impl AnswerFreshness {
    /// A sentence explaining the state, for the message shown to the user.
    pub fn explain(self, id: &str) -> String {
        match self {
            AnswerFreshness::Fresh => format!("{id} is answered"),
            AnswerFreshness::Missing => format!("{id} is unanswered"),
            AnswerFreshness::StaleQuestion => {
                format!("{id} changed after it was answered and must be answered again")
            }
            AnswerFreshness::StaleRecommendation => format!(
                "{id} was answered with REC and the recommendation has since changed; \
                 answer it again"
            ),
        }
    }
}

/// A behaviour the implementation must have.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requirement {
    /// `R<n>`.
    pub id: String,
    pub statement: String,
    /// The `C<n>` decisions this requirement came from.
    pub decision_ids: Vec<String>,
}

/// How a requirement is shown to hold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Acceptance {
    /// `A<n>`.
    pub id: String,
    pub requirement_ids: Vec<String>,
    /// What an observer sees when the requirement holds.
    pub observable: String,
}

/// One atomic implementation step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unit {
    /// `U<n>`.
    pub id: String,
    pub title: String,
    /// Repository-relative path prefixes this unit may change. Anything else is out of scope.
    pub paths: Vec<String>,
    pub acceptance_ids: Vec<String>,
    /// `U<n>` ids that must be accepted first.
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// A reversible experiment that resolves a `probe_required` decision. Probe units are run but
    /// never shipped.
    #[serde(default)]
    pub probe: bool,
}

/// A command whose exit status is the evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Test {
    /// `T<n>`.
    pub id: String,
    /// The exact command, run by the host and judged by its exit status.
    pub command: String,
    pub acceptance_ids: Vec<String>,
    /// The `U<n>` whose loop runs this test.
    pub unit_id: String,
}

/// Something that blocks freezing until it is resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenItem {
    pub id: String,
    /// The `C<n>` that produced it.
    pub decision_id: String,
    pub detail: String,
}

/// A recorded review pass over the plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanReview {
    /// The plan digest the review looked at. A review of a different digest is stale.
    pub plan_digest: Digest,
    pub ts: String,
    pub passed: bool,
    #[serde(default)]
    pub findings: Vec<String>,
}

/// The two plan reviews required before freezing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanReviews {
    /// Economy reading the plan cold: can a unit brief be written without a new product decision?
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cold_consumer: Option<PlanReview>,
    /// Critic's adversarial pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub critic: Option<PlanReview>,
}

/// A change the user asked for after seeing the draft pull request.
///
/// Kept on the plan rather than only echoed back, because the next planning round has to be able to
/// read it. An adjustment that only appears in a message is an adjustment the machine never saw.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Adjustment {
    /// The revision this feedback opened.
    pub revision: u32,
    /// The user's words, verbatim.
    pub text: String,
    pub ts: String,
}

/// An authorization seal: planning confirmation or the recorded direct BUILD instruction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frozen {
    /// The digest the challenge was derived from.
    pub digest: Digest,
    pub confirmed_at: String,
    /// The user's message, verbatim.
    pub answer_text: String,
}

/// The whole frozen contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub schema: String,
    /// An explicitly approved Codex plan, distinct from interview answers and typed confirmation.
    #[serde(deserialize_with = "crate::required_option")]
    pub approved_plan: Option<crate::approval::PlanApproval>,
    #[serde(deserialize_with = "crate::required_option")]
    pub execution_branch: Option<String>,
    /// Stop after confirmation; BUILD is a separate explicit action.
    pub plan_only: bool,
    /// Whether this run uses the answer-driven planning interview.
    pub interactive: bool,
    /// The whole ready frontier for this interview round, paged by the host UI.
    pub question_frontier: Vec<String>,
    /// Repository commit inspected by a new PLAN, fixed before user confirmation.
    #[serde(deserialize_with = "crate::required_option")]
    pub source_head: Option<String>,
    /// Verbatim instruction explicitly authorizing BUILD without the planning interview.
    #[serde(deserialize_with = "crate::required_option")]
    pub execution_authorization: Option<String>,
    /// Exact integration base for direct builds; a moving local branch is not the baseline.
    #[serde(deserialize_with = "crate::required_option")]
    pub base_commit: Option<String>,
    /// Stable slug identifying the run, and the `hwahap/<goal_id>` branch name.
    pub goal_id: String,
    /// Incremented by each adjustment round.
    pub revision: u32,
    pub base_branch: String,
    pub goal: Goal,
    /// Every surface appears, applicable or not. A missing surface is a validation failure, so a
    /// surface can never be skipped by omission.
    pub surfaces: std::collections::BTreeMap<String, SurfaceStatus>,
    pub facts: Vec<Fact>,
    pub decisions: Vec<Decision>,
    pub requirements: Vec<Requirement>,
    pub acceptance: Vec<Acceptance>,
    pub units: Vec<Unit>,
    pub tests: Vec<Test>,
    pub open_items: Vec<OpenItem>,
    /// What the user asked for after seeing a draft pull request, oldest first.
    pub adjustments: Vec<Adjustment>,
    /// Derived requirements, acceptance, units, and tests need regeneration from current inputs.
    pub structure_stale: bool,
    /// The command run once, after every unit is accepted.
    pub full_suite: String,
    pub reviews: PlanReviews,
    /// Set by `CONFIRM PLAN` or explicit BUILD; excluded from the plan digest.
    #[serde(deserialize_with = "crate::required_option")]
    pub frozen: Option<Frozen>,
}

/// What the user asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Goal {
    pub statement: String,
    #[serde(default)]
    pub success: Vec<String>,
    #[serde(default)]
    pub non_goals: Vec<String>,
}

impl Plan {
    /// A new plan with every surface applicable and nothing decided.
    pub fn new(
        goal_id: impl Into<String>,
        base_branch: impl Into<String>,
        statement: impl Into<String>,
    ) -> Self {
        Plan {
            schema: SCHEMA.to_string(),
            approved_plan: None,
            execution_branch: None,
            plan_only: false,
            interactive: false,
            question_frontier: Vec::new(),
            source_head: None,
            execution_authorization: None,
            base_commit: None,
            goal_id: goal_id.into(),
            revision: 1,
            base_branch: base_branch.into(),
            goal: Goal {
                statement: statement.into(),
                success: Vec::new(),
                non_goals: Vec::new(),
            },
            surfaces: SURFACES
                .iter()
                .map(|s| (s.id().to_string(), SurfaceStatus::Applicable))
                .collect(),
            facts: Vec::new(),
            decisions: Vec::new(),
            requirements: Vec::new(),
            acceptance: Vec::new(),
            units: Vec::new(),
            tests: Vec::new(),
            open_items: Vec::new(),
            adjustments: Vec::new(),
            structure_stale: false,
            full_suite: String::new(),
            reviews: PlanReviews::default(),
            frozen: None,
        }
    }

    /// The digest the `CONFIRM PLAN` challenge is derived from.
    ///
    /// Computed over the plan with `frozen` cleared, so confirming does not change the digest that
    /// was confirmed.
    pub fn digest(&self) -> Result<Digest> {
        let mut unfrozen = self.clone();
        unfrozen.frozen = None;
        Digest::of(&unfrozen)
    }

    /// The challenge string the user must type back.
    pub fn challenge(&self) -> Result<String> {
        Ok(self.digest()?.challenge())
    }

    /// The digest a plan review binds to.
    ///
    /// Computed with `reviews` and `frozen` cleared. Recording a review necessarily changes
    /// [`Plan::digest`], so a review that bound itself to that digest would be stale the instant it
    /// was stored. This digest covers everything a reviewer actually read and nothing else, so a
    /// review goes stale exactly when the reviewed content changes.
    pub fn review_digest(&self) -> Result<Digest> {
        let mut reviewed = self.clone();
        reviewed.frozen = None;
        reviewed.reviews = PlanReviews::default();
        Digest::of(&reviewed)
    }

    /// True when the plan was confirmed and has not changed since.
    pub fn is_frozen(&self) -> Result<bool> {
        match &self.frozen {
            None => Ok(false),
            Some(frozen) => Ok(frozen.digest == self.digest()?),
        }
    }

    /// Looks up a decision by `C<n>`.
    pub fn decision(&self, id: &str) -> Option<&Decision> {
        self.decisions.iter().find(|d| d.id == id)
    }

    /// Looks up a decision by `C<n>` for mutation.
    pub fn decision_mut(&mut self, id: &str) -> Option<&mut Decision> {
        self.decisions.iter_mut().find(|d| d.id == id)
    }

    /// Looks up a unit by `U<n>`.
    pub fn unit(&self, id: &str) -> Option<&Unit> {
        self.units.iter().find(|u| u.id == id)
    }

    /// The surfaces the user has not marked `NA`.
    pub fn applicable_surfaces(&self) -> Vec<Surface> {
        SURFACES
            .into_iter()
            .filter(|s| matches!(self.surfaces.get(s.id()), Some(SurfaceStatus::Applicable)))
            .collect()
    }

    /// The decisions on an applicable surface.
    pub fn decisions_on(&self, surface: Surface) -> Vec<&Decision> {
        self.decisions
            .iter()
            .filter(|d| d.surface == surface)
            .collect()
    }

    /// The tests belonging to a unit.
    pub fn tests_for(&self, unit_id: &str) -> Vec<&Test> {
        self.tests.iter().filter(|t| t.unit_id == unit_id).collect()
    }

    /// A digest of everything a unit was built to satisfy.
    ///
    /// Covers the unit itself, the acceptance criteria it delivers, the requirements behind them,
    /// the resolved value of every decision those requirements rest on, and the commands that prove
    /// it. An adjustment changes some of those and not others, and this is what tells the two
    /// apart: a unit whose fingerprint still matches is work the new revision did not touch, and
    /// rebuilding it would waste the user's time; a unit whose fingerprint moved was accepted
    /// against something that is no longer true.
    pub fn unit_fingerprint(&self, unit_id: &str) -> Result<Digest> {
        let unit = self
            .unit(unit_id)
            .ok_or_else(|| Error::Rejected(format!("{unit_id} is not a unit in this plan")))?;

        let acceptance: Vec<&Acceptance> = self
            .acceptance
            .iter()
            .filter(|a| unit.acceptance_ids.contains(&a.id))
            .collect();
        let requirements: Vec<&Requirement> = self
            .requirements
            .iter()
            .filter(|r| acceptance.iter().any(|a| a.requirement_ids.contains(&r.id)))
            .collect();
        let mut decisions: Vec<serde_json::Value> = Vec::new();
        for decision in &self.decisions {
            if !requirements
                .iter()
                .any(|r| r.decision_ids.contains(&decision.id))
            {
                continue;
            }
            decisions.push(serde_json::json!({
                "id": decision.id,
                "question": decision.question,
                "resolved": decision.resolved_value()?,
            }));
        }
        let mut tests: Vec<&Test> = self.tests_for(unit_id);
        tests.sort_by(|a, b| a.id.cmp(&b.id));

        Digest::of(&serde_json::json!({
            "unit": unit,
            "acceptance": acceptance,
            "requirements": requirements,
            "decisions": decisions,
            "tests": tests,
        }))
    }

    /// Rejects a plan whose schema tag is not `hwahap/v4`.
    ///
    /// Another schema is not imported: the shapes do not correspond, and a silent partial import
    /// would produce a plan the user never confirmed.
    pub fn require_supported_schema(&self) -> Result<()> {
        if self.schema != SCHEMA {
            return Err(Error::Rejected(format!(
                "this .hwahap directory holds schema {:?}, but Hwahap only supports {SCHEMA}. \
                 Retire the old active state and start a new run; Hwahap does not convert older runs.",
                self.schema
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_plan_requires_every_persisted_field() {
        let plan = Plan::new("goal", "main", "a goal");
        let saved = serde_json::to_value(&plan).unwrap();
        assert_eq!(saved["structure_stale"], false);
        assert_eq!(serde_json::from_value::<Plan>(saved.clone()).unwrap(), plan);
        for field in saved.as_object().unwrap().keys() {
            let mut missing = saved.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<Plan>(missing).is_err(), "{field}");
        }
        let mut extra = saved;
        extra["removed_field"] = true.into();
        assert!(serde_json::from_value::<Plan>(extra).is_err());
    }

    fn decision() -> Decision {
        Decision {
            id: "C1".into(),
            surface: Surface::S1,
            kind: DecisionKind::Decision,
            question: "Call the admission webhook during dry-run?".into(),
            alternatives: vec![
                Alternative {
                    id: "ALT1".into(),
                    value: "call it".into(),
                },
                Alternative {
                    id: "ALT2".into(),
                    value: "skip it".into(),
                },
            ],
            recommendation: Recommendation::Recommended {
                choice: "ALT1".into(),
                rationale: vec!["keeps validation parity with apply".into()],
                evidence: vec!["F7".into()],
                tradeoffs: vec!["a webhook failure becomes a dry-run failure".into()],
                impact: vec!["api".into(), "tests".into()],
                confidence: Confidence::High,
            },
            depends_on: vec![],
            answer: None,
        }
    }

    fn answer_with(d: &Decision, selection: Selection) -> Answer {
        Answer {
            text: "C1=REC".into(),
            recommendation: matches!(selection, Selection::Recommendation)
                .then(|| d.recommendation_digest().unwrap()),
            selection,
            ts: "2026-09-04T00:00:00Z".into(),
            identity: d.identity_digest().unwrap(),
        }
    }

    #[test]
    fn all_twelve_surfaces_are_present_and_distinct() {
        assert_eq!(SURFACES.len(), 12);
        let ids: std::collections::BTreeSet<_> = SURFACES.iter().map(|s| s.id()).collect();
        assert_eq!(ids.len(), 12);
        for surface in SURFACES {
            assert_eq!(Surface::parse(surface.id()), Some(surface));
            assert!(!surface.title().is_empty());
        }
        assert_eq!(Surface::parse("S13"), None);
        assert_eq!(Surface::parse("s1"), None);
        assert_eq!(Surface::parse(""), None);
    }

    #[test]
    fn a_recommendation_alone_leaves_the_decision_unanswered() {
        let d = decision();
        assert_eq!(d.answer_freshness().unwrap(), AnswerFreshness::Missing);
        assert!(!d.is_answered().unwrap());
        assert_eq!(d.resolved_value().unwrap(), None);
    }

    #[test]
    fn rec_resolves_to_the_recommended_alternative() {
        let mut d = decision();
        d.answer = Some(answer_with(&d, Selection::Recommendation));
        assert!(d.is_answered().unwrap());
        assert_eq!(d.resolved_value().unwrap().as_deref(), Some("call it"));
    }

    #[test]
    fn changing_the_recommendation_makes_a_rec_answer_stale() {
        let mut d = decision();
        d.answer = Some(answer_with(&d, Selection::Recommendation));
        d.recommendation = Recommendation::Recommended {
            choice: "ALT2".into(),
            rationale: vec!["webhook latency dominates".into()],
            evidence: vec!["F9".into()],
            tradeoffs: vec!["dry-run diverges from apply".into()],
            impact: vec!["api".into()],
            confidence: Confidence::Medium,
        };
        assert_eq!(
            d.answer_freshness().unwrap(),
            AnswerFreshness::StaleRecommendation
        );
        assert!(!d.is_answered().unwrap());
        assert_eq!(d.resolved_value().unwrap(), None);
    }

    #[test]
    fn changing_the_recommendation_leaves_an_explicit_alt_answer_fresh() {
        let mut d = decision();
        d.answer = Some(answer_with(
            &d,
            Selection::Alternative { id: "ALT2".into() },
        ));
        d.recommendation = Recommendation::NoRecommendation {
            rationale: vec!["tie".into()],
        };
        assert_eq!(d.answer_freshness().unwrap(), AnswerFreshness::Fresh);
        assert_eq!(d.resolved_value().unwrap().as_deref(), Some("skip it"));
    }

    #[test]
    fn rewording_the_question_makes_every_answer_stale() {
        for selection in [
            Selection::Recommendation,
            Selection::Alternative { id: "ALT1".into() },
            Selection::Other {
                value: "fail after 10s".into(),
            },
            Selection::Unknown,
        ] {
            let mut d = decision();
            d.answer = Some(answer_with(&d, selection.clone()));
            d.question = "Call the admission webhook during dry-run, ever?".into();
            assert_eq!(
                d.answer_freshness().unwrap(),
                AnswerFreshness::StaleQuestion,
                "selection {selection:?} should have gone stale"
            );
        }
    }

    #[test]
    fn adding_an_alternative_makes_every_answer_stale() {
        let mut d = decision();
        d.answer = Some(answer_with(
            &d,
            Selection::Alternative { id: "ALT1".into() },
        ));
        d.alternatives.push(Alternative {
            id: "ALT3".into(),
            value: "ask per call".into(),
        });
        assert_eq!(
            d.answer_freshness().unwrap(),
            AnswerFreshness::StaleQuestion
        );
    }

    #[test]
    fn unknown_is_answered_but_resolves_to_nothing() {
        let mut d = decision();
        d.answer = Some(answer_with(&d, Selection::Unknown));
        assert!(d.is_answered().unwrap());
        assert_eq!(d.resolved_value().unwrap(), None);
    }

    #[test]
    fn other_resolves_to_the_users_own_words() {
        let mut d = decision();
        d.answer = Some(answer_with(
            &d,
            Selection::Other {
                value: "fail after 10s".into(),
            },
        ));
        assert_eq!(
            d.resolved_value().unwrap().as_deref(),
            Some("fail after 10s")
        );
    }

    #[test]
    fn rec_against_a_non_recommending_mode_resolves_to_nothing() {
        for recommendation in [
            Recommendation::NoRecommendation {
                rationale: vec!["no basis".into()],
            },
            Recommendation::ProbeRequired {
                probe_unit: "U9".into(),
                rationale: vec!["measure".into()],
            },
        ] {
            let mut d = decision();
            d.recommendation = recommendation;
            d.answer = Some(answer_with(&d, Selection::Recommendation));
            assert!(d.is_answered().unwrap());
            assert_eq!(d.resolved_value().unwrap(), None);
        }
    }

    #[test]
    fn a_rec_answer_without_a_recorded_recommendation_digest_is_stale() {
        let mut d = decision();
        let mut answer = answer_with(&d, Selection::Recommendation);
        answer.recommendation = None;
        d.answer = Some(answer);
        assert_eq!(
            d.answer_freshness().unwrap(),
            AnswerFreshness::StaleRecommendation
        );
    }

    #[test]
    fn an_alt_answer_naming_an_unknown_alternative_resolves_to_nothing() {
        let mut d = decision();
        d.answer = Some(answer_with(
            &d,
            Selection::Alternative { id: "ALT9".into() },
        ));
        assert!(d.is_answered().unwrap());
        assert_eq!(d.resolved_value().unwrap(), None);
    }

    #[test]
    fn freezing_does_not_change_the_digest_that_was_frozen() {
        let mut plan = Plan::new("2026-09-04-dry-run", "main", "add dry-run");
        let digest = plan.digest().unwrap();
        plan.frozen = Some(Frozen {
            digest: digest.clone(),
            confirmed_at: "2026-09-04T00:00:00Z".into(),
            answer_text: format!("CONFIRM PLAN {}", digest.challenge()),
        });
        assert_eq!(plan.digest().unwrap(), digest);
        assert!(plan.is_frozen().unwrap());
    }

    #[test]
    fn any_change_after_freezing_unfreezes_the_plan() {
        let mut plan = Plan::new("g", "main", "goal");
        let digest = plan.digest().unwrap();
        plan.frozen = Some(Frozen {
            digest,
            confirmed_at: "2026-09-04T00:00:00Z".into(),
            answer_text: "CONFIRM PLAN".into(),
        });
        assert!(plan.is_frozen().unwrap());
        plan.goal.statement = "a different goal".into();
        assert!(!plan.is_frozen().unwrap());
    }

    #[test]
    fn the_digest_is_stable_across_serialization_round_trips() {
        let mut plan = Plan::new("g", "main", "goal");
        plan.decisions.push(decision());
        plan.facts.push(Fact {
            id: "F7".into(),
            question: "does apply call the webhook?".into(),
            answer: "yes".into(),
            sources: vec!["src/apply/mod.rs:120-146".into()],
        });
        let encoded = serde_json::to_string(&plan).unwrap();
        let decoded: Plan = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, plan);
        assert_eq!(decoded.digest().unwrap(), plan.digest().unwrap());
        assert_eq!(decoded.challenge().unwrap(), plan.challenge().unwrap());
    }

    #[test]
    fn a_new_plan_has_all_surfaces_applicable_and_nothing_decided() {
        let plan = Plan::new("g", "main", "goal");
        assert_eq!(plan.schema, SCHEMA);
        assert_eq!(plan.revision, 1);
        assert_eq!(plan.applicable_surfaces().len(), 12);
        assert!(plan.decisions.is_empty());
        assert!(!plan.is_frozen().unwrap());
        plan.require_supported_schema().unwrap();
    }

    #[test]
    fn marking_a_surface_na_removes_it_from_the_applicable_set() {
        let mut plan = Plan::new("g", "main", "goal");
        plan.surfaces.insert(
            "S9".into(),
            SurfaceStatus::NotApplicable {
                reason: "no released consumers".into(),
                answer: Answer {
                    text: "S9=NA".into(),
                    selection: Selection::NotApplicable,
                    ts: "2026-09-04T00:00:00Z".into(),
                    identity: Digest::zero(),
                    recommendation: None,
                },
            },
        );
        let applicable = plan.applicable_surfaces();
        assert_eq!(applicable.len(), 11);
        assert!(!applicable.contains(&Surface::S9));
    }

    #[test]
    fn a_v2_directory_is_rejected_rather_than_imported() {
        let mut plan = Plan::new("g", "main", "goal");
        plan.schema = "hwahap/v2".into();
        let err = plan.require_supported_schema().unwrap_err();
        let message = err.to_string();
        assert!(message.contains("hwahap/v2"), "{message}");
        assert!(message.contains("does not convert"), "{message}");
    }

    #[test]
    fn lookups_find_present_items_and_reject_absent_ones() {
        let mut plan = Plan::new("g", "main", "goal");
        plan.decisions.push(decision());
        plan.units.push(Unit {
            id: "U1".into(),
            title: "wire the flag".into(),
            paths: vec!["src/".into()],
            acceptance_ids: vec!["A1".into()],
            depends_on: vec![],
            probe: false,
        });
        plan.tests.push(Test {
            id: "T1".into(),
            command: "cargo test".into(),
            acceptance_ids: vec!["A1".into()],
            unit_id: "U1".into(),
        });
        assert_eq!(plan.decision("C1").map(|d| d.id.as_str()), Some("C1"));
        assert!(plan.decision("C2").is_none());
        assert!(plan.decision_mut("C1").is_some());
        assert_eq!(plan.unit("U1").map(|u| u.id.as_str()), Some("U1"));
        assert!(plan.unit("U2").is_none());
        assert_eq!(plan.tests_for("U1").len(), 1);
        assert!(plan.tests_for("U2").is_empty());
        assert_eq!(plan.decisions_on(Surface::S1).len(), 1);
        assert!(plan.decisions_on(Surface::S2).is_empty());
    }
}
