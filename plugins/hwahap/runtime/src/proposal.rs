//! What a planning agent is allowed to hand back, and how it becomes part of the plan.
//!
//! A planning agent proposes; it never commits. Everything here is validated before it touches
//! [`crate::plan::Plan`]: ids must be well-formed and must not collide with what already exists,
//! references must resolve, and a decision arrives with no answer attached. That last point is the
//! whole reason these are separate types — a proposal has no field in which an agent could record
//! a user answer that never happened.

use serde::{Deserialize, Serialize};

use crate::agentresult::parse_strict;
use crate::error::{Error, Result};
use crate::plan::{
    Acceptance, Alternative, Decision, DecisionKind, Fact, Plan, Recommendation, Requirement,
    Surface, Test, Unit,
};

/// Repository facts established by a read-only session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactsProposal {
    pub facts: Vec<Fact>,
}

impl FactsProposal {
    /// The exact shape the fact session must emit.
    pub const CONTRACT: &'static str =
        r#"{"facts":[{"id":"F1","question":"...","answer":"...","sources":["path:1-20"]}]}"#;

    /// Parses and validates a fact proposal against the plan it will extend.
    pub fn parse(final_message: &str, plan: &Plan) -> Result<FactsProposal> {
        let proposal: FactsProposal = parse_strict(final_message, Self::CONTRACT)?;
        let existing: Vec<&str> = plan.facts.iter().map(|f| f.id.as_str()).collect();
        check_ids("F", proposal.facts.iter().map(|f| f.id.as_str()), &existing)?;
        for fact in &proposal.facts {
            if fact.answer.trim().is_empty() {
                return Err(Error::Rejected(format!(
                    "fact {} has an empty answer",
                    fact.id
                )));
            }
            if fact.sources.is_empty() {
                return Err(Error::Rejected(format!(
                    "fact {} cites no source; an uncited fact cannot be checked",
                    fact.id
                )));
            }
            if fact.sources.iter().any(|source| source.trim().is_empty()) {
                return Err(Error::Rejected(format!(
                    "fact {} contains a blank source location",
                    fact.id
                )));
            }
        }
        Ok(proposal)
    }
}

/// A surface the planner believes does not apply, awaiting the user's `S<n>=NA`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposedNotApplicable {
    pub surface: String,
    pub reason: String,
}

/// A decision put to the user, with no answer attached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposedDecision {
    pub id: String,
    pub surface: String,
    pub kind: DecisionKind,
    pub question: String,
    pub alternatives: Vec<Alternative>,
    pub recommendation: Recommendation,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

impl ProposedDecision {
    fn into_decision(self) -> Result<Decision> {
        let surface = Surface::parse(&self.surface).ok_or_else(|| {
            Error::Rejected(format!(
                "decision {} names the surface {:?}, which is not one of S1..S12",
                self.id, self.surface
            ))
        })?;
        Ok(Decision {
            id: self.id,
            surface,
            kind: self.kind,
            question: self.question,
            alternatives: self.alternatives,
            recommendation: self.recommendation,
            depends_on: self.depends_on,
            // A proposal has no answer, and there is no path by which it could acquire one here.
            answer: None,
        })
    }
}

/// The next round of questions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionsProposal {
    #[serde(default)]
    pub decisions: Vec<ProposedDecision>,
    #[serde(default)]
    pub not_applicable: Vec<ProposedNotApplicable>,
}

impl DecisionsProposal {
    /// The exact shape the decision session must emit.
    pub const CONTRACT: &'static str = concat!(
        r#"{"decisions":[{"id":"C1","surface":"S1","kind":"decision|scenario|term","question":"...","#,
        r#""alternatives":[{"id":"ALT1","value":"..."},{"id":"ALT2","value":"..."}],"#,
        r#""recommendation":{"mode":"recommended","choice":"ALT1","rationale":["..."],"evidence":["F1"],"#,
        r#""tradeoffs":["..."],"impact":["..."],"confidence":"high"},"depends_on":[]}],"#,
        r#""not_applicable":[{"surface":"S9","reason":"..."}]}"#
    );

    /// Parses, validates, and converts into decisions ready to append to the plan.
    pub fn parse(
        final_message: &str,
        plan: &Plan,
    ) -> Result<(Vec<Decision>, Vec<ProposedNotApplicable>)> {
        let proposal: DecisionsProposal = parse_strict(final_message, Self::CONTRACT)?;
        let existing: Vec<&str> = plan
            .decisions
            .iter()
            .filter(|d| {
                !plan
                    .open_items
                    .iter()
                    .any(|o| o.id == format!("CLARIFY-{}", d.id))
            })
            .map(|d| d.id.as_str())
            .collect();
        check_ids(
            "C",
            proposal.decisions.iter().map(|d| d.id.as_str()),
            &existing,
        )?;

        for na in &proposal.not_applicable {
            if Surface::parse(&na.surface).is_none() {
                return Err(Error::Rejected(format!(
                    "{:?} is not one of S1..S12",
                    na.surface
                )));
            }
            if na.reason.trim().is_empty() {
                return Err(Error::Rejected(format!(
                    "surface {} is proposed as not applicable with no reason; the user cannot \
                     confirm what they cannot read",
                    na.surface
                )));
            }
        }

        let known_facts: Vec<&str> = plan.facts.iter().map(|f| f.id.as_str()).collect();
        let mut decisions = Vec::with_capacity(proposal.decisions.len());
        for proposed in proposal.decisions {
            let decision = proposed.into_decision()?;
            let missing = missing_recommendation_fields(&decision.recommendation);
            if !missing.is_empty() {
                return Err(Error::Rejected(format!(
                    "decision {} needs nonempty, nonblank recommendation fields: {}",
                    decision.id,
                    missing.join(", ")
                )));
            }
            if decision.alternatives.len() < 2 {
                return Err(Error::Rejected(format!(
                    "decision {} offers fewer than two alternatives, so it is not a question",
                    decision.id
                )));
            }
            if let Recommendation::Recommended {
                choice, evidence, ..
            } = &decision.recommendation
            {
                if !decision.alternatives.iter().any(|a| &a.id == choice) {
                    return Err(Error::Rejected(format!(
                        "decision {} recommends {choice:?}, which is not one of its alternatives",
                        decision.id
                    )));
                }
                for fact in evidence {
                    if !known_facts.contains(&fact.as_str()) {
                        return Err(Error::Rejected(format!(
                            "decision {} cites {fact:?} as evidence, but no such fact exists",
                            decision.id
                        )));
                    }
                }
            }
            decisions.push(decision);
        }
        Ok((decisions, proposal.not_applicable))
    }
}

/// Presence checks only: readable explanations are required, but their truth and relevance need
/// evidence review. A nonempty string is not a machine proof of a recommendation's merits.
pub(crate) fn missing_recommendation_fields(recommendation: &Recommendation) -> Vec<&'static str> {
    let fields: Vec<(&str, &[String])> = match recommendation {
        Recommendation::Recommended {
            rationale,
            evidence,
            tradeoffs,
            impact,
            ..
        } => vec![
            ("rationale", rationale),
            ("evidence", evidence),
            ("tradeoffs", tradeoffs),
            ("impact", impact),
        ],
        Recommendation::NoRecommendation { rationale }
        | Recommendation::ProbeRequired { rationale, .. } => vec![("rationale", rationale)],
    };
    fields
        .into_iter()
        .filter(|(_, values)| {
            values.is_empty() || values.iter().any(|value| value.trim().is_empty())
        })
        .map(|(name, _)| name)
        .collect()
}

/// The requirement, acceptance, unit and test graph derived from answered decisions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructureProposal {
    pub requirements: Vec<Requirement>,
    pub acceptance: Vec<Acceptance>,
    pub units: Vec<Unit>,
    pub tests: Vec<Test>,
    /// The command run once after every unit is accepted.
    pub full_suite: String,
}

impl StructureProposal {
    /// The exact shape the structure session must emit.
    pub const CONTRACT: &'static str = concat!(
        r#"{"requirements":[{"id":"R1","statement":"...","decision_ids":["C1"]}],"#,
        r#""acceptance":[{"id":"A1","requirement_ids":["R1"],"observable":"..."}],"#,
        r#""units":[{"id":"U1","title":"...","paths":["src/"],"acceptance_ids":["A1"],"depends_on":[],"probe":false}],"#,
        r#""tests":[{"id":"T1","command":"...","acceptance_ids":["A1"],"unit_id":"U1"}],"#,
        r#""full_suite":"..."}"#
    );

    /// Parses and validates the structure. Cross-references into the plan are checked here; the
    /// freeze gate re-checks everything from the assembled plan, so this is a fast rejection, not
    /// the authority.
    pub fn parse(final_message: &str, plan: &Plan) -> Result<StructureProposal> {
        let proposal: StructureProposal = parse_strict(final_message, Self::CONTRACT)?;
        check_ids(
            "R",
            proposal.requirements.iter().map(|r| r.id.as_str()),
            &[],
        )?;
        check_ids("A", proposal.acceptance.iter().map(|a| a.id.as_str()), &[])?;
        check_ids("U", proposal.units.iter().map(|u| u.id.as_str()), &[])?;
        check_ids("T", proposal.tests.iter().map(|t| t.id.as_str()), &[])?;

        if proposal.full_suite.trim().is_empty() {
            return Err(Error::Rejected(
                "full_suite is empty; there would be nothing to run once the units are done".into(),
            ));
        }
        let known_decisions: Vec<&str> = plan.decisions.iter().map(|d| d.id.as_str()).collect();
        for requirement in &proposal.requirements {
            if requirement.decision_ids.is_empty() {
                return Err(Error::Rejected(format!(
                    "requirement {} cites no decision, so nothing the user chose supports it",
                    requirement.id
                )));
            }
            for id in &requirement.decision_ids {
                if !known_decisions.contains(&id.as_str()) {
                    return Err(Error::Rejected(format!(
                        "requirement {} cites {id:?}, which is not a decision in this plan",
                        requirement.id
                    )));
                }
            }
        }
        Ok(proposal)
    }
}

/// Rejects malformed ids, duplicates within the proposal, and collisions with what exists.
fn check_ids<'a>(
    prefix: &str,
    ids: impl Iterator<Item = &'a str>,
    existing: &[&str],
) -> Result<()> {
    let mut seen: Vec<&str> = Vec::new();
    for id in ids {
        if !well_formed(prefix, id) {
            return Err(Error::Rejected(format!(
                "{id:?} is not a well-formed identifier; it must be {prefix} followed by a decimal \
                 number of at least 1 with no leading zero"
            )));
        }
        if seen.contains(&id) {
            return Err(Error::Rejected(format!(
                "{id:?} appears twice in one proposal"
            )));
        }
        if existing.contains(&id) {
            return Err(Error::Rejected(format!(
                "{id:?} already exists in the plan; a proposal may not reuse an identifier"
            )));
        }
        seen.push(id);
    }
    Ok(())
}

fn well_formed(prefix: &str, id: &str) -> bool {
    let Some(number) = id.strip_prefix(prefix) else {
        return false;
    };
    !number.is_empty()
        && number.bytes().all(|b| b.is_ascii_digit())
        && !number.starts_with('0')
        && number.parse::<u32>().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_with_a_fact() -> Plan {
        let mut plan = Plan::new("g", "main", "goal");
        plan.facts.push(Fact {
            id: "F1".into(),
            question: "does apply call the webhook?".into(),
            answer: "yes".into(),
            sources: vec!["src/apply.rs:10-20".into()],
        });
        plan
    }

    fn a_decision(id: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "surface": "S1",
            "kind": "decision",
            "question": "Should dry-run call the existing admission webhook?",
            "alternatives": [{"id": "ALT1", "value": "call the webhook"}, {"id": "ALT2", "value": "skip the webhook"}],
            "recommendation": {
                "mode": "recommended", "choice": "ALT1", "rationale": ["reuse apply's admission validation"],
                "evidence": ["F1"], "tradeoffs": ["webhook failures also fail dry-run"], "impact": ["api validation"], "confidence": "high"
            },
            "depends_on": []
        })
    }

    #[test]
    fn a_well_formed_decision_proposal_converts() {
        let plan = plan_with_a_fact();
        let message = serde_json::json!({"decisions": [a_decision("C1")]}).to_string();
        let (decisions, na) = DecisionsProposal::parse(&message, &plan).unwrap();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].id, "C1");
        assert_eq!(decisions[0].surface, Surface::S1);
        assert!(na.is_empty());
    }

    #[test]
    fn explanation_fields_cannot_be_omitted_or_padded_with_blank_entries() {
        let plan = plan_with_a_fact();
        for field in ["rationale", "evidence", "tradeoffs", "impact"] {
            for invalid in [serde_json::json!([]), serde_json::json!(["F1", " \u{a0}"])] {
                let mut decision = a_decision("C1");
                decision["recommendation"][field] = invalid;
                let err = DecisionsProposal::parse(
                    &serde_json::json!({"decisions": [decision]}).to_string(),
                    &plan,
                )
                .unwrap_err();
                assert!(err.to_string().contains(field), "{err}");
            }
        }
        for mode in ["no_recommendation", "probe_required"] {
            let mut decision = a_decision("C1");
            decision["recommendation"] = serde_json::json!({"mode": mode, "rationale": []});
            if mode == "probe_required" {
                decision["recommendation"]["probe_unit"] = "U1".into();
            }
            let err = DecisionsProposal::parse(
                &serde_json::json!({"decisions": [decision]}).to_string(),
                &plan,
            )
            .unwrap_err();
            assert!(err.to_string().contains("rationale"), "{err}");
        }
    }

    #[test]
    fn a_source_list_cannot_hide_a_blank_citation_beside_a_real_one() {
        let plan = Plan::new("g", "main", "goal");
        let err = FactsProposal::parse(
            &serde_json::json!({"facts": [{"id": "F1", "question": "where is apply?",
                "answer": "src/apply.rs", "sources": ["src/apply.rs:10-20", " \u{a0}"]}]})
            .to_string(),
            &plan,
        )
        .unwrap_err();
        assert!(err.to_string().contains("blank source"), "{err}");
    }

    #[test]
    fn a_proposed_decision_can_never_arrive_pre_answered() {
        let plan = plan_with_a_fact();
        let (decisions, _) = DecisionsProposal::parse(
            &serde_json::json!({"decisions": [a_decision("C1")]}).to_string(),
            &plan,
        )
        .unwrap();
        assert!(decisions[0].answer.is_none());
        assert!(!decisions[0].is_answered().unwrap());

        // And an agent cannot smuggle one in: `answer` is not a field of the proposal type.
        let mut sneaky = a_decision("C2");
        sneaky["answer"] = serde_json::json!({"text": "C2=REC", "selection": {"kind": "recommendation"},
            "ts": "2026-09-04T00:00:00Z", "identity": "sha256:00"});
        let err = DecisionsProposal::parse(
            &serde_json::json!({"decisions": [sneaky]}).to_string(),
            &plan,
        )
        .unwrap_err();
        assert!(err.to_string().contains("answer"), "{err}");
    }

    #[test]
    fn a_decision_id_that_already_exists_is_rejected() {
        let mut plan = plan_with_a_fact();
        let (mut decisions, _) = DecisionsProposal::parse(
            &serde_json::json!({"decisions": [a_decision("C1")]}).to_string(),
            &plan,
        )
        .unwrap();
        plan.decisions.append(&mut decisions);

        let err = DecisionsProposal::parse(
            &serde_json::json!({"decisions": [a_decision("C1")]}).to_string(),
            &plan,
        )
        .unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
    }

    #[test]
    fn duplicate_ids_within_one_proposal_are_rejected() {
        let plan = plan_with_a_fact();
        let err = DecisionsProposal::parse(
            &serde_json::json!({"decisions": [a_decision("C1"), a_decision("C1")]}).to_string(),
            &plan,
        )
        .unwrap_err();
        assert!(err.to_string().contains("twice"), "{err}");
    }

    #[test]
    fn malformed_ids_are_rejected() {
        let plan = plan_with_a_fact();
        for id in [
            "C0",
            "C01",
            "c1",
            "C",
            "C-1",
            "C1a",
            "X1",
            "",
            "C4294967296",
        ] {
            let err = DecisionsProposal::parse(
                &serde_json::json!({"decisions": [a_decision(id)]}).to_string(),
                &plan,
            )
            .unwrap_err();
            assert!(err.to_string().contains("well-formed"), "{id} -> {err}");
        }
    }

    #[test]
    fn a_recommendation_naming_an_absent_alternative_is_rejected() {
        let plan = plan_with_a_fact();
        let mut decision = a_decision("C1");
        decision["recommendation"]["choice"] = serde_json::json!("ALT9");
        let err = DecisionsProposal::parse(
            &serde_json::json!({"decisions": [decision]}).to_string(),
            &plan,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("not one of its alternatives"),
            "{err}"
        );
    }

    #[test]
    fn a_recommendation_citing_an_absent_fact_is_rejected() {
        let plan = plan_with_a_fact();
        let mut decision = a_decision("C1");
        decision["recommendation"]["evidence"] = serde_json::json!(["F9"]);
        let err = DecisionsProposal::parse(
            &serde_json::json!({"decisions": [decision]}).to_string(),
            &plan,
        )
        .unwrap_err();
        assert!(err.to_string().contains("no such fact"), "{err}");
    }

    #[test]
    fn a_decision_with_fewer_than_two_alternatives_is_rejected() {
        let plan = plan_with_a_fact();
        let mut decision = a_decision("C1");
        decision["alternatives"] = serde_json::json!([{"id": "ALT1", "value": "a"}]);
        decision["recommendation"]["choice"] = serde_json::json!("ALT1");
        let err = DecisionsProposal::parse(
            &serde_json::json!({"decisions": [decision]}).to_string(),
            &plan,
        )
        .unwrap_err();
        assert!(err.to_string().contains("not a question"), "{err}");
    }

    #[test]
    fn an_unknown_surface_is_rejected_for_decisions_and_for_na() {
        let plan = plan_with_a_fact();
        let mut decision = a_decision("C1");
        decision["surface"] = serde_json::json!("S13");
        let err = DecisionsProposal::parse(
            &serde_json::json!({"decisions": [decision]}).to_string(),
            &plan,
        )
        .unwrap_err();
        assert!(err.to_string().contains("S1..S12"), "{err}");

        let err = DecisionsProposal::parse(
            &serde_json::json!({"not_applicable": [{"surface": "S0", "reason": "x"}]}).to_string(),
            &plan,
        )
        .unwrap_err();
        assert!(err.to_string().contains("S1..S12"), "{err}");
    }

    #[test]
    fn a_not_applicable_surface_without_a_reason_is_rejected() {
        let plan = plan_with_a_fact();
        for reason in ["", "   "] {
            let err = DecisionsProposal::parse(
                &serde_json::json!({"not_applicable": [{"surface": "S9", "reason": reason}]})
                    .to_string(),
                &plan,
            )
            .unwrap_err();
            assert!(err.to_string().contains("no reason"), "{err}");
        }
    }

    #[test]
    fn an_uncited_or_empty_fact_is_rejected() {
        let plan = Plan::new("g", "main", "goal");
        let err = FactsProposal::parse(
            &serde_json::json!({"facts": [{"id": "F1", "question": "q", "answer": "a", "sources": []}]})
                .to_string(),
            &plan,
        )
        .unwrap_err();
        assert!(err.to_string().contains("cites no source"), "{err}");

        let err = FactsProposal::parse(
            &serde_json::json!({"facts": [{"id": "F1", "question": "q", "answer": " ", "sources": ["a:1"]}]})
                .to_string(),
            &plan,
        )
        .unwrap_err();
        assert!(err.to_string().contains("empty answer"), "{err}");
    }

    #[test]
    fn a_well_formed_fact_proposal_parses() {
        let plan = Plan::new("g", "main", "goal");
        let proposal = FactsProposal::parse(
            &serde_json::json!({"facts": [
                {"id": "F1", "question": "q", "answer": "a", "sources": ["src/x.rs:1-4"]}
            ]})
            .to_string(),
            &plan,
        )
        .unwrap();
        assert_eq!(proposal.facts.len(), 1);
        assert_eq!(proposal.facts[0].sources, vec!["src/x.rs:1-4"]);
    }

    #[test]
    fn a_structure_proposal_must_ground_every_requirement_in_a_real_decision() {
        let mut plan = plan_with_a_fact();
        let (decisions, _) = DecisionsProposal::parse(
            &serde_json::json!({"decisions": [a_decision("C1")]}).to_string(),
            &plan,
        )
        .unwrap();
        plan.decisions = decisions;

        let ok = serde_json::json!({
            "requirements": [{"id": "R1", "statement": "s", "decision_ids": ["C1"]}],
            "acceptance": [{"id": "A1", "requirement_ids": ["R1"], "observable": "o"}],
            "units": [{"id": "U1", "title": "t", "paths": ["src/"], "acceptance_ids": ["A1"], "depends_on": [], "probe": false}],
            "tests": [{"id": "T1", "command": "c", "acceptance_ids": ["A1"], "unit_id": "U1"}],
            "full_suite": "cargo test"
        });
        StructureProposal::parse(&ok.to_string(), &plan).unwrap();

        let mut dangling = ok.clone();
        dangling["requirements"][0]["decision_ids"] = serde_json::json!(["C9"]);
        let err = StructureProposal::parse(&dangling.to_string(), &plan).unwrap_err();
        assert!(
            err.to_string().contains("not a decision in this plan"),
            "{err}"
        );

        let mut ungrounded = ok.clone();
        ungrounded["requirements"][0]["decision_ids"] = serde_json::json!([]);
        let err = StructureProposal::parse(&ungrounded.to_string(), &plan).unwrap_err();
        assert!(err.to_string().contains("cites no decision"), "{err}");

        let mut no_suite = ok;
        no_suite["full_suite"] = serde_json::json!("  ");
        let err = StructureProposal::parse(&no_suite.to_string(), &plan).unwrap_err();
        assert!(err.to_string().contains("full_suite is empty"), "{err}");
    }

    #[test]
    fn every_contract_is_valid_json() {
        for contract in [
            FactsProposal::CONTRACT,
            DecisionsProposal::CONTRACT,
            StructureProposal::CONTRACT,
        ] {
            let value: serde_json::Value = serde_json::from_str(contract).unwrap();
            assert!(value.is_object(), "{contract}");
        }
    }

    #[test]
    fn prose_around_a_proposal_is_rejected_like_any_other_result() {
        let plan = Plan::new("g", "main", "goal");
        let err = FactsProposal::parse("Here you go: {\"facts\":[]}", &plan).unwrap_err();
        assert!(
            err.to_string().contains("did not start with a JSON object"),
            "{err}"
        );
    }

    #[test]
    fn well_formed_accepts_only_the_documented_shape() {
        assert!(well_formed("C", "C1"));
        assert!(well_formed("C", "C12"));
        assert!(well_formed("U", "U999"));
        assert!(!well_formed("C", "U1"));
        assert!(!well_formed("C", "C0"));
        assert!(!well_formed("C", "C01"));
        assert!(!well_formed("C", "C"));
        assert!(!well_formed("C", "C1.0"));
        assert!(!well_formed("C", "C１"));
    }
}
