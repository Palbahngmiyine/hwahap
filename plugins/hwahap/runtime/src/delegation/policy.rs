use super::*;
use crate::catalog::{CatalogSnapshot, HostObservation};
use std::collections::BTreeSet;

pub struct Context {
    pub run_id: String,
    pub contract_digest: String,
    pub role: Role,
    pub unit: Option<String>,
    pub completed: BTreeSet<String>,
    pub write_paths: Vec<String>,
    pub shared_state: bool,
    pub writer_available: bool,
    pub bound: Option<(String, String)>,
    pub reviewer_bindings: std::collections::BTreeMap<NativeLane, (String, String)>,
    pub free_slots: u32,
    pub preflight_verified: bool,
}
fn sorted(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values.dedup();
    values
}
pub fn decide(
    snapshot: &CatalogSnapshot,
    host: &HostObservation,
    assessment: Option<&TaskAssessment>,
    context: &Context,
) -> Result<DelegationDecision> {
    snapshot.validate(&context.run_id)?;
    let mut decision = DelegationDecision {
        route: Route::Wait,
        lane: NativeLane::for_role(context.role),
        selection: None,
        assessment_digest: assessment.map(TaskAssessment::digest).transpose()?,
        task_digest: assessment.map(TaskAssessment::task_digest).transpose()?,
        catalog_digest: snapshot.digest.to_string(),
        host_digest: Some(host.digest()?.to_string()),
        obligations: Obligations {
            independent_review: true,
            review_depth: Depth::Deep,
            recovery_validation: false,
        },
        reason_codes: vec![],
        digest: String::new(),
    };
    let Some(assessment) = assessment else {
        return finish(decision, Route::Wait, "assessment_missing");
    };
    if assessment.validate().is_err() {
        return finish(decision, Route::Wait, "assessment_missing");
    }
    if assessment.run_id != context.run_id
        || assessment.contract_digest != context.contract_digest
        || assessment.role != context.role
        || assessment.unit != context.unit
    {
        return finish(decision, Route::Replan, "assessment_binding_changed");
    }
    let writer = matches!(context.role, Role::Implementer | Role::Rework);
    if writer
        && (assessment.topology.writer_owner.as_deref()
            != Some(context.unit.as_deref().unwrap_or(&context.run_id))
            || sorted(assessment.topology.write_paths.clone())
                != sorted(context.write_paths.clone()))
    {
        return finish(decision, Route::Replan, "writer_conflict");
    }
    if !context.writer_available && writer {
        return finish(decision, Route::Wait, "writer_busy");
    }
    if assessment
        .topology
        .predecessors
        .iter()
        .any(|p| !context.completed.contains(p))
    {
        return finish(decision, Route::Wait, "predecessor_pending");
    }
    let high = assessment.risk.high()?;
    decision.obligations.recovery_validation = high;
    decision.obligations.review_depth = if high { Depth::Deep } else { Depth::Focused };
    let requirements = assessment.merged(&snapshot.catalog.role_requirements[&context.role])?;
    let tools = crate::catalog::tools_for(context.role);
    let mut review_slots = 0;
    if writer {
        for role in [Role::UnitReviewer, Role::FinalReview] {
            let mut review = assessment.clone();
            review.role = role;
            let needed = review.merged(&snapshot.catalog.role_requirements[&role])?;
            let lane = NativeLane::for_role(role);
            let tools = crate::catalog::tools_for(role);
            if let Some((model, effort)) = context.reviewer_bindings.get(&lane) {
                if !snapshot.catalog.supports(model, effort, &needed) {
                    return finish(decision, Route::Wait, "bound_capability_insufficient");
                }
                if !host.available(model, effort, &tools) {
                    return finish(decision, Route::Wait, "model_unavailable");
                }
            } else {
                if !snapshot
                    .catalog
                    .candidates(&needed)
                    .into_iter()
                    .any(|(m, e)| host.available(m, e, &tools))
                {
                    return finish(decision, Route::Wait, "model_unavailable");
                }
                review_slots += 1;
            }
        }
        if context.free_slots < review_slots {
            return finish(decision, Route::Wait, "slot_unavailable");
        }
    }

    let independent = matches!(decision.lane, NativeLane::Critic | NativeLane::Auditor);
    let shared = context.shared_state || assessment.topology.coupling == Coupling::Shared;
    let parent_required = !independent && decision.lane == NativeLane::Coordinator;
    if high && writer && !context.preflight_verified {
        return finish(decision, Route::Wait, "recovery_validation_required");
    }
    let mut reason = if shared {
        "shared_state"
    } else if high {
        "high_risk"
    } else {
        "capability_match"
    };
    let pair = if parent_required {
        decision.lane = NativeLane::Coordinator;
        (host.parent_model.clone(), host.parent_effort.clone())
    } else if let Some(pair) = &context.bound {
        if snapshot.catalog.supports(&pair.0, &pair.1, &requirements) {
            pair.clone()
        } else if independent {
            return finish(decision, Route::Wait, "bound_capability_insufficient");
        } else {
            decision.lane = NativeLane::Coordinator;
            reason = "bound_capability_insufficient";
            (host.parent_model.clone(), host.parent_effort.clone())
        }
    } else {
        if context.free_slots <= review_slots {
            if writer
                && snapshot
                    .catalog
                    .supports(&host.parent_model, &host.parent_effort, &requirements)
                && host.available(&host.parent_model, &host.parent_effort, &tools)
            {
                decision.lane = NativeLane::Coordinator;
                reason = "reuse_parent_capacity";
                (host.parent_model.clone(), host.parent_effort.clone())
            } else {
                return finish(decision, Route::Wait, "slot_unavailable");
            }
        } else {
            let Some((model, effort)) = snapshot
                .catalog
                .candidates(&requirements)
                .into_iter()
                .find(|(m, e)| host.available(m, e, &tools))
            else {
                return finish(decision, Route::Wait, "model_unavailable");
            };
            (model.to_owned(), effort.to_owned())
        }
    };
    if !snapshot.catalog.supports(&pair.0, &pair.1, &requirements) {
        return finish(decision, Route::Wait, "bound_capability_insufficient");
    }
    if !host.available(&pair.0, &pair.1, &tools) {
        return finish(decision, Route::Wait, "model_unavailable");
    }
    decision.selection = Some(Selection::for_requirements(
        snapshot,
        host,
        context.role,
        context.unit.clone(),
        &pair.0,
        &pair.1,
        requirements,
    )?);
    let route = if decision.lane == NativeLane::Coordinator {
        Route::Coordinator
    } else {
        Route::Worker
    };
    finish(decision, route, reason)
}
fn finish(
    mut decision: DelegationDecision,
    route: Route,
    reason: &str,
) -> Result<DelegationDecision> {
    decision.route = route;
    decision.reason_codes.push(reason.into());
    decision.seal()?;
    Ok(decision)
}
