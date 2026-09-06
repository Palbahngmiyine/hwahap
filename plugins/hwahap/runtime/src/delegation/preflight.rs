use super::*;
use crate::{git::Git, session::SessionReceipt, state::Store, verification};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preflight {
    pub task_digest: String,
    pub author_head: String,
    pub author_code: Digest,
    pub reviews: Vec<SessionReceipt>,
    pub verifications: Vec<verification::VerificationRequest>,
}
fn name(task: &str, head: &str) -> String {
    format!(
        "preflight-{}.json",
        Digest::of_bytes(format!("{task}\0{head}").as_bytes())
    )
}
pub fn verified(store: &Store, assessment: &TaskAssessment) -> Result<bool> {
    if !store.worktree_path().exists() {
        return Ok(false);
    }
    let head = Git::open(&store.worktree_path())?
        .run_in(&store.worktree_path(), &["rev-parse", "HEAD"])?;
    let Some(proof) = crate::pr_review::read_evidence::<Preflight>(
        store,
        &name(&assessment.task_digest()?, &head),
    )?
    else {
        return Ok(false);
    };
    validate(store, assessment, &proof)?;
    Ok(true)
}
pub fn save(store: &Store, assessment: &TaskAssessment, proof: &Preflight) -> Result<()> {
    validate(store, assessment, proof)?;
    crate::pr_review::save_evidence(store, &name(&proof.task_digest, &proof.author_head), proof)
}
fn validate(store: &Store, assessment: &TaskAssessment, proof: &Preflight) -> Result<()> {
    let reject = || {
        Error::Rejected("recovery_validation_required: current independent reviews and isolated recovery evidence are required".into())
    };
    let recovery = assessment.recovery.as_ref().ok_or_else(reject)?;
    let plan = store.read_plan()?.ok_or_else(reject)?;
    let head = Git::open(&store.worktree_path())?
        .run_in(&store.worktree_path(), &["rev-parse", "HEAD"])?;
    if proof.task_digest != assessment.task_digest()?
        || proof.author_head != head
        || proof.author_code
            != Git::open(&store.worktree_path())?.fingerprint(&store.worktree_path())?
        || proof.reviews.len() != 2
        || proof.verifications.len() != 2
    {
        return Err(reject());
    }
    let snapshot = crate::catalog::snapshot(store, &assessment.run_id)?;
    let mut identities = std::collections::BTreeSet::new();
    for (receipt, role) in proof
        .reviews
        .iter()
        .zip([Role::UnitReviewer, Role::FinalReview])
    {
        receipt.verify()?;
        let SessionReceipt::Native(native) = receipt;
        let mut wanted = assessment.clone();
        wanted.role = role;
        if native.role != role
            || native.assessment.as_ref() != Some(&wanted)
            || native.agent_id == "coordinator"
            || !identities.insert(&native.agent_id)
        {
            return Err(reject());
        }
        native.selection.verify_catalog(&snapshot)?;
        let request: crate::native::NativeDispatch = crate::pr_review::read_evidence(
            store,
            &format!("native-request-{}.json", native.dispatch_id),
        )?
        .ok_or_else(reject)?;
        let completion: crate::native::NativeCompletion = crate::pr_review::read_evidence(
            store,
            &format!("native-completion-{}.json", native.dispatch_id),
        )?
        .ok_or_else(reject)?;
        request.verify_decision()?;
        if request.base_head != proof.author_head
            || request.assessment != wanted
            || request.selection != native.selection
            || completion.agent_id != native.agent_id
            || !completion.agent_stopped
            || completion.decision_digest.as_deref() != Some(&request.decision.digest)
        {
            return Err(reject());
        }
        let envelope: serde_json::Value =
            serde_json::from_str(&completion.final_message).map_err(|_| reject())?;
        if envelope["dispatch_id"] != native.dispatch_id || envelope["result"]["verdict"] != "pass"
        {
            return Err(reject());
        }
    }
    for (request, command) in proof
        .verifications
        .iter()
        .zip([&recovery.failure_command, &recovery.recovery_command])
    {
        if request.kind != verification::Kind::Preflight
            || request.command != *command
            || request.cwd != recovery.environment
            || request.run_id != assessment.run_id
            || request.unit_id != assessment.unit
            || request.contract
                != match assessment.unit.as_deref() {
                    Some(id) => plan.unit_fingerprint(id)?,
                    None => plan.digest()?,
                }
            || request.head != head
        {
            return Err(reject());
        }
        let environment = std::path::Path::new(&request.cwd);
        let git = Git::open(environment)?;
        if git.run_in(environment, &["rev-parse", "HEAD"])? != request.head
            || git.fingerprint(environment)? != request.code
            || verification::inputs::digest(environment, &plan.verification_inputs)?
                != request.inputs
        {
            return Err(reject());
        }
        verification::require_current_verification(store, request)?;
    }
    Ok(())
}
