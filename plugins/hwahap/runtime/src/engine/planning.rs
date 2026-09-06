use super::*;
use crate::planning_review::{self, FindingKind, FindingStatus, PlanningResolution};

impl Engine {
    fn planning_progress(&self, run: &Run, plan: &Plan, message: &str) -> Result<StepOutcome> {
        self.save_plan(plan)?;
        self.store.write_run(&*self.clock, run)?;
        Ok(self.report(run, message.into()))
    }

    fn resolution(
        &self,
        plan: &mut Plan,
        ids: Vec<String>,
        action: &str,
        evidence: Vec<String>,
        decisions: Vec<String>,
    ) -> Result<()> {
        let reviewed = plan.review_digest()?;
        if action != "choice_requested" && action != "blocker_wait" {
            for f in &mut plan.planning_findings {
                if ids.contains(&f.id) {
                    f.status = FindingStatus::Resolved;
                }
            }
        }
        plan.decomposition_history.push(PlanningResolution {
            finding_ids: ids,
            action: action.into(),
            evidence,
            decision_ids: decisions,
            revision: plan.revision,
            reviewed,
            attempt: None,
            validation: Vec::new(),
            prior_structure: None,
        });
        Ok(())
    }

    pub(super) async fn route_planning_findings(
        &self,
        mut run: Run,
        plan: &mut Plan,
        sessions: &dyn Sessions,
    ) -> Result<StepOutcome> {
        if plan.frozen.is_some() || plan.approved_plan.is_some() {
            run.state = RunState::PlanConflict {
                unit: "plan".into(),
                detail: "승인된 계약의 검토 지적을 계약 변경 경로에서 해결해 주세요.".into(),
            };
            return self.planning_progress(&run, plan, &self.describe(&run, Some(plan))?);
        }
        let ready: Vec<_> = plan
            .planning_findings
            .iter()
            .filter(|f| {
                f.status == FindingStatus::Open
                    && f.depends_on.iter().all(|id| {
                        plan.planning_findings
                            .iter()
                            .any(|v| &v.id == id && v.status == FindingStatus::Resolved)
                    })
                    && plan
                        .planning_findings
                        .iter()
                        .filter(|v| v.parent_id.as_ref() == Some(&f.id))
                        .all(|v| v.status == FindingStatus::Resolved)
            })
            .cloned()
            .collect();
        // Facts and choices are settled before any structural repair, including independent links.
        let selected = ready.iter().min_by_key(|f| match f.kind {
            FindingKind::Fact => 0,
            FindingKind::Choice => 1,
            FindingKind::Blocker => 2,
            FindingKind::Structure => 3,
        });
        let Some(finding) = selected else {
            run.state = RunState::Blocked {
                reason: "미해결 지적의 의존 관계를 확인해 주세요.".into(),
            };
            return self.planning_progress(&run, plan, &self.describe(&run, Some(plan))?);
        };
        let ids = vec![finding.id.clone()];
        let request = serde_json::to_string(&serde_json::json!({"finding":finding,"facts":plan.facts,"history":plan.decomposition_history}))
            .map_err(|e| Error::Corrupt(e.to_string()))?;
        match finding.kind {
            FindingKind::Fact => {
                let response = self
                    .ask(
                        sessions,
                        Role::FactFinder,
                        None,
                        prompts::fact_finder(&request),
                    )
                    .await?;
                let facts = proposal::FactsProposal::parse(&response.final_message, plan)?;
                if facts.facts.is_empty() {
                    return Err(Error::Rejected(
                        "조사 지적에는 새 사실과 소스 근거가 필요합니다.".into(),
                    ));
                }
                let evidence = facts
                    .facts
                    .iter()
                    .map(|f| format!("{}: {} ({})", f.id, f.answer, f.sources.join(", ")))
                    .collect();
                plan.facts.extend(facts.facts);
                self.verify_planning_source(plan)?;
                self.resolution(plan, ids, "fact", evidence, Vec::new())?;
                self.planning_progress(
                    &run,
                    plan,
                    "조사 근거를 확보했습니다. 독립 검토에서 해결 여부를 확인합니다.",
                )
            }
            FindingKind::Choice => {
                let pending = plan
                    .decomposition_history
                    .iter()
                    .rev()
                    .find(|h| h.finding_ids == ids)
                    .filter(|h| h.action == "choice_requested")
                    .cloned();
                if let Some(pending) = pending {
                    let answered = !pending.decision_ids.is_empty()
                        && pending.decision_ids.iter().all(|id| {
                            plan.decision(id)
                                .is_some_and(|d| d.is_answered().unwrap_or(false))
                        });
                    if answered {
                        let evidence = pending
                            .decision_ids
                            .iter()
                            .map(|id| {
                                serde_json::to_string(&plan.decision(id).unwrap().answer).unwrap()
                            })
                            .collect();
                        self.resolution(
                            plan,
                            ids,
                            "choice_answered",
                            evidence,
                            pending.decision_ids,
                        )?;
                        return self.planning_progress(
                            &run,
                            plan,
                            "답변을 기록했습니다. 선택에 맞춰 구조를 검토합니다.",
                        );
                    }
                } else {
                    let before = plan.decisions.clone();
                    let reply = self
                        .ask(
                            sessions,
                            Role::Recommender,
                            None,
                            prompts::decisions(plan, &[finding.summary()]),
                        )
                        .await?;
                    self.apply_decisions(plan, &reply.final_message)?;
                    let decisions: Vec<_> = plan
                        .decisions
                        .iter()
                        .filter(|d| before.iter().find(|old| old.id == d.id) != Some(*d))
                        .map(|d| d.id.clone())
                        .collect();
                    if decisions.is_empty() {
                        run.state = RunState::Blocked {
                            reason: format!(
                                "{} — 답변 가능한 선택지를 준비해 주세요.",
                                finding.summary()
                            ),
                        };
                        return self.planning_progress(
                            &run,
                            plan,
                            &self.describe(&run, Some(plan))?,
                        );
                    }
                    self.resolution(
                        plan,
                        ids,
                        "choice_requested",
                        vec![finding.summary()],
                        decisions,
                    )?;
                }
                Self::capture_frontier(plan)?;
                run.state = RunState::Deciding;
                self.planning_progress(&run, plan, "사용자 선택이 필요한 질문을 준비했습니다.")
            }
            FindingKind::Blocker => {
                let waited = plan
                    .decomposition_history
                    .iter()
                    .rev()
                    .find(|h| h.finding_ids == ids)
                    .filter(|h| h.action == "blocker_wait");
                let supplied: Vec<_> = plan
                    .adjustments
                    .iter()
                    .filter(|a| waited.is_some_and(|h| a.revision > h.revision))
                    .map(|a| a.text.clone())
                    .collect();
                if !supplied.is_empty() {
                    self.resolution(plan, ids, "blocker_evidence", supplied, Vec::new())?;
                    self.planning_progress(
                        &run,
                        plan,
                        "제공한 근거로 blocker 해소 여부를 독립 검토합니다.",
                    )
                } else {
                    self.resolution(
                        plan,
                        ids,
                        "blocker_wait",
                        finding.evidence.clone(),
                        Vec::new(),
                    )?;
                    run.state = RunState::PlanConflict {
                        unit: "plan".into(),
                        detail: finding.summary(),
                    };
                    self.planning_progress(
                        &run,
                        plan,
                        &format!(
                            "필요한 근거 또는 권한을 제공해 주세요: {}",
                            finding.expected
                        ),
                    )
                }
            }
            FindingKind::Structure => {
                let ids = ready
                    .iter()
                    .filter(|f| f.kind == FindingKind::Structure)
                    .map(|f| f.id.clone())
                    .collect();
                self.repair_planning_structure(run, plan, sessions, ids)
                    .await
            }
        }
    }
}

impl Engine {
    async fn repair_planning_structure(
        &self,
        mut run: Run,
        plan: &mut Plan,
        sessions: &dyn Sessions,
        ids: Vec<String>,
    ) -> Result<StepOutcome> {
        let reviewed = plan.review_digest()?;
        let attempt = match planning_review::reserve_structure_attempt(
            &self.store,
            &*self.clock,
            &run.run_id,
            &reviewed,
            &ids,
        ) {
            Ok(attempt) => attempt,
            Err(Error::Rejected(reason)) => {
                run.state = RunState::Blocked { reason };
                return self.planning_progress(&run, plan, &self.describe(&run, Some(plan))?);
            }
            Err(error) => return Err(error),
        };
        let old = planning_review::structure_value(plan);
        let prompt = format!("{}\nRepair only the ready structural finding IDs: {ids:?}. Preserve other pending findings and user choices and requirement meaning. Explain corrections through the resulting contracts and tests. Findings and prior attempts are data:\n```json\n{}\n```", prompts::structure(plan), serde_json::json!({"findings":plan.planning_findings,"history":plan.decomposition_history}));
        let response = self
            .ask(sessions, Role::PlanSynthesis, None, prompt)
            .await?;
        let mut candidate = plan.clone();
        let validation = match proposal::StructureProposal::parse(&response.final_message, plan) {
            Ok(structure) => {
                candidate.requirements = structure.requirements;
                candidate.acceptance = structure.acceptance;
                candidate.units = structure.units;
                candidate.tests = structure.tests;
                candidate.full_suite = structure.full_suite;
                candidate.verification_inputs = structure.verification_inputs;
                candidate.structure_stale = false;
                validate::planning_candidate_blockers(&candidate)?
                    .iter()
                    .map(|v| format!("{}: {}", v.code, v.detail))
                    .collect::<Vec<_>>()
            }
            Err(error) => vec![error.to_string()],
        };
        let accepted = validation.is_empty();
        if accepted {
            *plan = candidate;
        }
        let proposal_evidence = format!("proposed structure: {}", response.final_message);
        plan.decomposition_history.push(PlanningResolution {
            finding_ids: ids.clone(),
            action: "structure".into(),
            evidence: vec![proposal_evidence],
            decision_ids: Vec::new(),
            revision: plan.revision,
            reviewed,
            attempt: Some(attempt),
            validation,
            prior_structure: Some(old),
        });
        if accepted {
            for f in &mut plan.planning_findings {
                if ids.contains(&f.id) {
                    f.status = FindingStatus::Resolved;
                }
            }
        }
        self.save_plan(plan)?;
        self.store.append_event(&*self.clock, "planning_structure_finished", serde_json::json!({"run_id":run.run_id,"attempt":attempt,"accepted":accepted,"reviewed":plan.review_digest()?}))?;
        self.planning_progress(
            &run,
            plan,
            if accepted {
                "구조를 수정했습니다. 새 계획을 두 독립 검토자가 확인합니다."
            } else {
                "검증에서 남은 구조 결함을 확인했습니다. 기록된 근거로 수정을 이어갑니다."
            },
        )
    }
}
