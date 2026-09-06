use super::*;
impl Engine {
    pub(super) async fn high_risk_preflight(
        &self,
        plan: &Plan,
        unit: Option<&str>,
        author_role: Role,
        sessions: &dyn Sessions,
    ) -> Result<()> {
        let Some(assessment) = crate::delegation::store::load(&self.store, author_role, unit)?
        else {
            return Ok(());
        };
        if !assessment.risk.high()?
            || crate::delegation::preflight::verified(&self.store, &assessment)?
        {
            return Ok(());
        }
        let recovery = assessment.recovery.as_ref().ok_or_else(|| Error::Rejected("recovery_validation_required: provide an approved isolated environment and failure/recovery test commands".into()))?;
        if [
            &recovery.environment,
            &recovery.user_authorization,
            &recovery.failure_command,
            &recovery.recovery_command,
        ]
        .iter()
        .any(|s| s.trim().is_empty())
        {
            return Err(Error::Rejected(
                "recovery_validation_required: recovery fields must be complete".into(),
            ));
        }
        let environment = Path::new(&recovery.environment);
        let isolated = std::fs::canonicalize(environment).map_err(|e| Error::io(environment, e))?;
        let worktree = std::fs::canonicalize(self.store.worktree_path())
            .map_err(|e| Error::io(self.store.worktree_path(), e))?;
        let root =
            std::fs::canonicalize(&self.repo_root).map_err(|e| Error::io(&self.repo_root, e))?;
        if !environment.is_absolute()
            || isolated.starts_with(&worktree)
            || worktree.starts_with(&isolated)
            || isolated.starts_with(&root)
            || root.starts_with(&isolated)
        {
            return Err(Error::Rejected(
                "recovery environment must be a separate disposable checkout".into(),
            ));
        }
        let head = self.git.run_in(&worktree, &["rev-parse", "HEAD"])?;
        if self.git.run_in(environment, &["rev-parse", "HEAD"])? != head {
            return Err(Error::Rejected(
                "recovery checkout must match the current author HEAD".into(),
            ));
        }
        if !self.git.is_clean(environment)? || !self.git.is_clean(&worktree)? {
            return Err(Error::Rejected(
                "preflight requires clean author and isolated checkouts".into(),
            ));
        }
        let mut reviews = Vec::new();
        for (role, focus) in [
            (
                Role::UnitReviewer,
                "impact, authority and shared-state coupling",
            ),
            (
                Role::FinalReview,
                "recovery procedure and isolated environment",
            ),
        ] {
            let prompt = format!("Before implementation, independently review {focus}. Verify source evidence, the quoted user authorization, the disposable environment, and the exact failure and recovery test commands. Each command must return zero only when its claimed check passes. Return {{\"verdict\":\"pass\"|\"fail\",\"findings\":[...]}}. Task assessment: {}", prompts::quoted(&serde_json::to_string(&assessment).map_err(|e| Error::Internal(e.to_string()))?));
            let outcome = self
                .ask(sessions, role, unit.map(str::to_owned), prompt)
                .await?;
            let review = ReviewResult::parse(&outcome.final_message)?;
            if review.verdict == Verdict::Fail {
                return Err(Error::Rejected(format!(
                    "preflight review: {}",
                    review.findings.join("; ")
                )));
            }
            reviews.push(outcome.receipt);
        }
        let mut verifications = Vec::new();
        for command in [&recovery.failure_command, &recovery.recovery_command] {
            let output = self
                .run_verified_command(
                    plan,
                    unit,
                    None,
                    crate::verification::Kind::Preflight,
                    command,
                    environment,
                )
                .await?;
            if !output.success {
                return Err(Error::Rejected(format!(
                    "isolated recovery check failed: {}",
                    tail(&output.combined, 2000)
                )));
            }
            verifications.push(self.verification_request(
                plan,
                unit,
                None,
                crate::verification::Kind::Preflight,
                command,
                environment,
            )?);
        }
        crate::delegation::preflight::save(
            &self.store,
            &assessment,
            &crate::delegation::preflight::Preflight {
                task_digest: assessment.task_digest()?,
                author_head: head,
                author_code: self.git.fingerprint(&worktree)?,
                reviews,
                verifications,
            },
        )
    }
}
