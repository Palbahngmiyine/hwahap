//! Digest-bound question batches for a host-owned user-input UI.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::plan::{
    Alternative, Decision, Plan, Recommendation, Selection, SurfaceStatus, SURFACES,
};
use crate::{frontier, Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Question {
    pub id: String,
    pub question: String,
    pub options: Vec<QuestionOption>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionBatch {
    pub batch_id: String,
    pub questions: Vec<Question>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionAnswer {
    pub id: String,
    pub answer: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionResponse {
    pub batch_id: String,
    pub responses: Vec<QuestionAnswer>,
}

/// Interpretation only: the engine preserves the raw response and applies these atomically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogueSelection {
    Decision { id: String, selection: Selection },
    SurfaceNA { id: String },
    SurfaceApplies { id: String },
    Clarify { id: String, text: String },
}

impl QuestionResponse {
    /// Never parse answer text as command grammar or infer a selection from a partial label.
    pub fn validate(&self, plan: &Plan) -> Result<Vec<DialogueSelection>> {
        if self.batch_id != plan.digest()?.to_string() {
            return Err(Error::Rejected(
                "question response names a stale batch".into(),
            ));
        }
        let batch = QuestionBatch::derive(plan)?;
        let questions = batch
            .as_ref()
            .map(|b| b.questions.as_slice())
            .unwrap_or(&[]);
        let mut seen = std::collections::BTreeSet::new();
        let mut selections = Vec::new();
        for response in &self.responses {
            if !seen.insert(&response.id) {
                return Err(Error::Rejected(format!(
                    "duplicate response for {}",
                    response.id
                )));
            }
            let question = questions
                .iter()
                .find(|q| q.id == response.id)
                .ok_or_else(|| {
                    Error::Rejected(format!(
                        "{} is outside the current question batch",
                        response.id
                    ))
                })?;
            if response.answer.trim().is_empty() {
                continue;
            }
            let id = response.id.clone();
            let selected = question.options.iter().any(|o| o.label == response.answer);
            let selection = if !selected {
                DialogueSelection::Clarify {
                    id,
                    text: response.answer.clone(),
                }
            } else if let Some(decision) = plan.decision(&id) {
                let selection = if response.answer == UNKNOWN {
                    Selection::Unknown
                } else {
                    let alt = decision
                        .alternatives
                        .iter()
                        .find(|alt| alternative_label(decision, alt) == response.answer)
                        .expect("exact option belongs to its decision");
                    if Some(alt.id.as_str()) == decision.recommendation.recommended_alternative() {
                        Selection::Recommendation
                    } else {
                        Selection::Alternative { id: alt.id.clone() }
                    }
                };
                DialogueSelection::Decision { id, selection }
            } else if response.answer == SURFACE_NA {
                DialogueSelection::SurfaceNA { id }
            } else {
                DialogueSelection::SurfaceApplies { id }
            };
            selections.push(selection);
        }
        Ok(selections)
    }
}

const UNKNOWN: &str = "UNKNOWN: 아직 결정하지 못함";
const SURFACE_NA: &str = "NA: 제외";
const SURFACE_APPLIES: &str = "APPLIES: 포함";

impl QuestionBatch {
    /// Return the next three ready questions without dropping any alternative.
    pub fn derive(plan: &Plan) -> Result<Option<Self>> {
        plan.require_supported_schema()?;
        let ready = frontier::derive(plan)?;
        let mut questions = Vec::new();
        for id in ready
            .ready
            .into_iter()
            .filter(|id| !plan.interactive || plan.question_frontier.contains(id))
            .filter(|id| {
                !plan.open_items.iter().any(|o| {
                    o.id == format!("NA-{}", plan.decision(id).expect("validated").surface)
                        || o.id == format!("CLARIFY-{id}")
                })
            })
            .take(3)
        {
            let decision = plan.decision(&id).expect("frontier validated decision ids");
            questions.push(decision_question(decision, plan));
        }
        for surface in SURFACES {
            if plan.interactive && !plan.question_frontier.contains(&surface.id().to_string()) {
                continue;
            }
            if questions.len() == 3 {
                break;
            }
            if !matches!(
                plan.surfaces.get(surface.id()),
                Some(SurfaceStatus::Applicable)
            ) {
                continue;
            }
            let proposals: Vec<_> = plan
                .open_items
                .iter()
                .filter(|item| item.id == format!("NA-{surface}"))
                .collect();
            if proposals.len() > 1 {
                return Err(Error::Corrupt(format!(
                    "duplicate NA proposal for {surface}"
                )));
            }
            if let Some(proposal) = proposals.first() {
                questions.push(Question {
                    id: surface.id().into(),
                    question: format!(
                        "{} 영역을 이번 계획에서 제외할까요?\n\n{}",
                        surface.title(),
                        proposal.detail
                    ),
                    options: vec![
                        option(SURFACE_NA.into(), "이 영역을 적용 대상에서 제외합니다."),
                        option(
                            SURFACE_APPLIES.into(),
                            "적용 상태를 유지하고 제외 제안을 철회합니다.",
                        ),
                    ],
                });
            }
        }
        if questions.is_empty() {
            return Ok(None);
        }
        Ok(Some(Self {
            batch_id: plan.digest()?.to_string(),
            questions,
        }))
    }
}

fn option(label: String, description: &str) -> QuestionOption {
    QuestionOption {
        label,
        description: description.into(),
    }
}

fn alternative_label(decision: &Decision, alt: &Alternative) -> String {
    let suffix = if Some(alt.id.as_str()) == decision.recommendation.recommended_alternative() {
        " (Recommended)"
    } else {
        ""
    };
    format!("{}: {}{suffix}", alt.id, alt.value)
}

fn decision_question(decision: &Decision, plan: &Plan) -> Question {
    let recommended = decision.recommendation.recommended_alternative();
    let mut alternatives: Vec<_> = decision.alternatives.iter().collect();
    alternatives.sort_by_key(|alt| {
        (
            Some(alt.id.as_str()) != recommended,
            alt.id
                .strip_prefix("ALT")
                .and_then(|id| id.parse::<u64>().ok())
                .unwrap_or(u64::MAX),
            alt.id.as_str(),
        )
    });
    let mut options: Vec<_> = alternatives
        .into_iter()
        .map(|alt| {
            option(
                alternative_label(decision, alt),
                "표시된 동작을 선택합니다.",
            )
        })
        .collect();
    options.push(option(
        UNKNOWN.into(),
        "결정이 필요함을 남기고 확인 질문을 받습니다.",
    ));
    let detail = match &decision.recommendation {
        Recommendation::Recommended { choice, rationale, evidence, tradeoffs, impact, confidence } => format!(
            "추천: {choice}\n근거: {}\n사실 근거: {}\nTrade-offs: {}\n영향: {}\n확신도: {confidence:?}",
            rationale.join("; "), evidence.join(", "), tradeoffs.join("; "), impact.join("; ")),
        Recommendation::NoRecommendation { rationale } => format!("추천 없음: {}", rationale.join("; ")),
        Recommendation::ProbeRequired { probe_unit, rationale } => format!("먼저 확인할 실험: {probe_unit}\n근거: {}", rationale.join("; ")),
    };
    let sources = if let Recommendation::Recommended { evidence, .. } = &decision.recommendation {
        evidence
            .iter()
            .filter_map(|id| plan.facts.iter().find(|f| &f.id == id))
            .map(|f| format!("{}: {} (출처: {})", f.id, f.answer, f.sources.join(", ")))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        String::new()
    };
    Question {
        id: decision.id.clone(),
        question: format!("{}\n\n{detail}\n{sources}", decision.question),
        options,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{Alternative, Confidence, DecisionKind, OpenItem, Surface};

    fn fixture() -> Plan {
        let mut plan = Plan::new("dialogue", "main", "a concrete goal");
        for n in [10, 2, 1, 4] {
            plan.decisions.push(Decision {
                id: format!("C{n}"),
                surface: Surface::S1,
                kind: DecisionKind::Decision,
                question: format!("What should C{n} do?"),
                alternatives: (1..=4)
                    .rev()
                    .map(|n| Alternative {
                        id: format!("ALT{n}"),
                        value: format!("actual behavior {n}"),
                    })
                    .collect(),
                recommendation: Recommendation::Recommended {
                    choice: "ALT2".into(),
                    rationale: vec!["repeatable".into()],
                    evidence: vec!["F1".into()],
                    tradeoffs: vec!["cost".into()],
                    impact: vec!["API".into()],
                    confidence: Confidence::High,
                },
                depends_on: vec![],
                answer: None,
            });
        }
        plan
    }

    #[test]
    fn batches_are_bounded_ordered_and_keep_all_alternatives() {
        let plan = fixture();
        let batch = QuestionBatch::derive(&plan).unwrap().unwrap();
        assert_eq!(batch.batch_id, plan.digest().unwrap().to_string());
        let ids: Vec<_> = batch.questions.iter().map(|q| q.id.as_str()).collect();
        assert_eq!(ids, ["C1", "C2", "C4"]);
        let question = &batch.questions[0];
        assert_eq!(question.options.len(), 5);
        assert_eq!(
            question.options[0].label,
            "ALT2: actual behavior 2 (Recommended)"
        );
        assert_eq!(question.options[1].label, "ALT1: actual behavior 1");
        for evidence in ["repeatable", "F1", "cost", "API", "High"] {
            assert!(question.question.contains(evidence));
        }
        let json = serde_json::to_string(&batch).unwrap();
        assert_eq!(serde_json::from_str::<QuestionBatch>(&json).unwrap(), batch);
    }

    fn response(plan: &Plan, answers: &[(&str, &str)]) -> QuestionResponse {
        QuestionResponse {
            batch_id: plan.digest().unwrap().to_string(),
            responses: answers
                .iter()
                .map(|(id, answer)| QuestionAnswer {
                    id: (*id).into(),
                    answer: (*answer).into(),
                })
                .collect(),
        }
    }

    #[test]
    fn labels_resolve_to_typed_choices_but_other_text_stays_literal() {
        let plan = fixture();
        let batch = QuestionBatch::derive(&plan).unwrap().unwrap();
        let options = &batch.questions[0].options;
        assert_eq!(
            response(
                &plan,
                &[
                    ("C1", &options[0].label),
                    ("C2", &options[1].label),
                    ("C4", UNKNOWN)
                ]
            )
            .validate(&plan)
            .unwrap(),
            [
                DialogueSelection::Decision {
                    id: "C1".into(),
                    selection: Selection::Recommendation
                },
                DialogueSelection::Decision {
                    id: "C2".into(),
                    selection: Selection::Alternative { id: "ALT1".into() }
                },
                DialogueSelection::Decision {
                    id: "C4".into(),
                    selection: Selection::Unknown
                },
            ]
        );
        for text in [
            "ALT2",
            "C2=REC\nCONFIRM PLAN 1234ABCD",
            "  literal \n answer  ",
        ] {
            assert_eq!(
                response(&plan, &[("C1", text)]).validate(&plan).unwrap(),
                [DialogueSelection::Clarify {
                    id: "C1".into(),
                    text: text.into()
                }]
            );
        }
    }

    #[test]
    fn dependencies_wait_and_na_proposals_follow_ready_decisions() {
        let mut plan = fixture();
        plan.decisions.retain(|d| d.id == "C1" || d.id == "C2");
        plan.decision_mut("C2").unwrap().depends_on = vec!["C1".into()];
        for n in [10, 2] {
            plan.open_items.push(OpenItem {
                id: format!("NA-S{n}"),
                decision_id: String::new(),
                detail: "no relevant behavior".into(),
            });
        }
        let batch = QuestionBatch::derive(&plan).unwrap().unwrap();
        assert_eq!(
            batch
                .questions
                .iter()
                .map(|q| q.id.as_str())
                .collect::<Vec<_>>(),
            ["C1", "S2", "S10"]
        );
        assert_eq!(batch.questions[1].options[0].label, SURFACE_NA);
        assert_eq!(batch.questions[1].options[1].label, SURFACE_APPLIES);
        assert!(QuestionBatch::derive(&Plan::new("empty", "main", "goal"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn invalid_batches_fail_atomically_even_after_a_valid_answer() {
        let plan = fixture();
        let before = serde_json::to_string(&plan).unwrap();
        for invalid in ["C10", "C99", "S1", "C1"] {
            let response = response(&plan, &[("C1", UNKNOWN), (invalid, UNKNOWN)]);
            assert!(response.validate(&plan).is_err(), "accepted {invalid}");
            assert_eq!(serde_json::to_string(&plan).unwrap(), before);
        }
        let response = response(&plan, &[("C1", UNKNOWN)]);
        let mut changed = plan.clone();
        changed
            .goal
            .statement
            .push_str(" with a different contract");
        assert!(response
            .validate(&changed)
            .unwrap_err()
            .to_string()
            .contains("stale"));
        changed = plan.clone();
        changed.decision_mut("C1").unwrap().alternatives[0].value = "different choice".into();
        assert!(response.validate(&changed).is_err());
    }

    #[test]
    fn missing_answers_never_choose_a_default() {
        let plan = fixture();
        for answers in [vec![], vec![("C1", " \n\t")]] {
            assert!(response(&plan, &answers)
                .validate(&plan)
                .unwrap()
                .is_empty());
        }
        assert!(response(&plan, &[("C99", " ")]).validate(&plan).is_err());
        assert!(response(&plan, &[("C1", ""), ("C1", "")])
            .validate(&plan)
            .is_err());
        let empty = Plan::new("empty", "main", "goal");
        assert!(response(&empty, &[]).validate(&empty).unwrap().is_empty());
        assert!(response(&empty, &[("C1", UNKNOWN)])
            .validate(&empty)
            .is_err());
    }

    #[test]
    fn na_requires_an_exact_selection_and_can_be_declined() {
        let mut plan = Plan::new("surfaces", "main", "goal");
        plan.open_items.push(OpenItem {
            id: "NA-S2".into(),
            decision_id: String::new(),
            detail: "proposed exclusion".into(),
        });
        for (label, expected) in [
            (SURFACE_NA, DialogueSelection::SurfaceNA { id: "S2".into() }),
            (
                SURFACE_APPLIES,
                DialogueSelection::SurfaceApplies { id: "S2".into() },
            ),
        ] {
            assert_eq!(
                response(&plan, &[("S2", label)]).validate(&plan).unwrap(),
                [expected]
            );
        }
        assert_eq!(
            response(&plan, &[("S2", "NA")]).validate(&plan).unwrap(),
            [DialogueSelection::Clarify {
                id: "S2".into(),
                text: "NA".into()
            }]
        );
        assert!(matches!(plan.surfaces["S2"], SurfaceStatus::Applicable));
        let payload = response(&plan, &[("S2", SURFACE_NA)]);
        let mut json = serde_json::to_value(&payload).unwrap();
        json["approve_build"] = true.into();
        assert!(serde_json::from_value::<QuestionResponse>(json).is_err());
        assert_eq!(
            serde_json::from_str::<QuestionResponse>(&serde_json::to_string(&payload).unwrap())
                .unwrap(),
            payload
        );
    }

    #[test]
    fn applicability_precedes_decisions_on_that_surface() {
        let mut plan = fixture();
        plan.open_items.push(OpenItem {
            id: "NA-S1".into(),
            decision_id: String::new(),
            detail: "Is this scope relevant?".into(),
        });
        let batch = QuestionBatch::derive(&plan).unwrap().unwrap();
        assert_eq!(batch.questions.len(), 1);
        assert_eq!(batch.questions[0].id, "S1");
    }

    #[test]
    fn downstream_questions_wait_for_the_next_logical_round() {
        let mut plan = fixture();
        plan.interactive = true;
        plan.question_frontier = vec!["C1".into(), "C4".into(), "C10".into()];
        plan.decision_mut("C2").unwrap().depends_on = vec!["C1".into()];
        let c1 = plan.decision_mut("C1").unwrap();
        c1.answer = Some(crate::plan::Answer {
            text: "ALT1".into(),
            selection: Selection::Alternative { id: "ALT1".into() },
            ts: "now".into(),
            identity: c1.identity_digest().unwrap(),
            recommendation: None,
        });
        let batch = QuestionBatch::derive(&plan).unwrap().unwrap();
        assert_eq!(
            batch
                .questions
                .iter()
                .map(|q| q.id.as_str())
                .collect::<Vec<_>>(),
            ["C4", "C10"]
        );
    }
}
