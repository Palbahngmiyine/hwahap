//! `plan.md`: the document the user actually reads before typing `CONFIRM PLAN <challenge>`.
//!
//! The challenge is a digest of the plan, so the document and the digest must move together. If the
//! same plan could render two different documents, the user would be confirming a digest computed
//! from content they did not see. Everything here is therefore a pure function of the plan: no
//! clock, no environment, and no iteration order that the file layout could leak into.
//!
//! Two consequences shape the code below:
//! - Identifier sets (`evidence`, `depends_on`, `paths`, acceptance/requirement cross-references,
//!   and the id-keyed collections themselves) are sorted before they are printed, because their
//!   order carries no meaning and would otherwise make the bytes depend on the order the file
//!   happened to store them in. Prose lists (rationale, trade-offs, success criteria, findings) keep
//!   the author's order, because there the order *is* meaning.
//! - Rendering refuses to fail on a plan that is merely wrong. A cyclic unit graph or a dangling
//!   dependency is exactly what the user opened `plan.md` to find, so those degrade to a documented
//!   fallback. Rendering fails only when the plan is self-inconsistent in a way that would make the
//!   document lie: a missing or unknown surface row, or a schema this renderer does not implement.
//!
//! What may be frozen is asked of [`crate::validate::freeze_blockers`], never re-derived here. It is
//! the predicate the engine gates the real freeze on, and a second implementation of it would let
//! this document invite a confirmation the gate refuses — or withhold one the gate would have taken.
//! `unit_order` is the one thing computed locally, because `validate::unit_order` errs on a cycle
//! and a cycle is exactly what the reader opened `plan.md` to find.

use std::collections::BTreeMap;

use crate::canonical::Digest;
use crate::error::{Error, Result};
use crate::plan::{
    AnswerFreshness, Confidence, Decision, DecisionKind, Plan, PlanReview, Recommendation, Surface,
    SurfaceStatus, Unit, SURFACES,
};
use crate::validate;

/// Renders the plan the user reviews before freezing. Byte-stable for a given plan.
pub fn plan_markdown(plan: &Plan) -> Result<String> {
    plan.require_supported_schema()?;
    let mut md = Md::default();
    render_header(plan, &mut md)?;
    if plan.interactive {
        md.line(if plan.plan_only { "Execution: PLAN only. Confirmation saves this plan; BUILD requires a later explicit request." }
            else { "Execution: PLAN then BUILD. Confirmation starts implementation, tests and draft review." });
        if let Some(head) = &plan.source_head {
            md.line(format!("Inspected source commit: `{head}`"));
        }
        md.line("Recommendations and review judgments remain inspectable claims, not proof that every requirement has been discovered.");
    }
    render_goal(plan, &mut md);
    if plan.execution_authorization.is_none() {
        render_surfaces(plan, &mut md)?;
    }
    render_facts(plan, &mut md);
    render_decisions(plan, &mut md)?;
    render_requirements(plan, &mut md);
    render_acceptance(plan, &mut md);
    render_units(plan, &mut md);
    render_tests(plan, &mut md);
    render_open_items(plan, &mut md);
    if let Some(approval) = &plan.approved_plan {
        md.line("## Approved Codex plan");
        md.line("The original implementation request is recorded separately from CONFIRM PLAN. Contract translation requires independent review; no interview answers are fabricated.");
        md.line(format!(
            "Approved document: `{}`; source: `{}`",
            approval.markdown_digest, approval.source_head
        ));
        for line in approval.markdown.lines() {
            md.line(format!("> {line}"));
        }
        render_reviews(plan, &mut md)?;
    } else if plan.execution_authorization.is_some() {
        md.line("Planning omitted under the recorded explicit BUILD instruction. No planning reviews or CONFIRM PLAN receipt are claimed.");
    } else {
        render_reviews(plan, &mut md)?;
        render_confirm(plan, &mut md)?;
    }
    Ok(md.finish())
}

/// Renders the decision frontier put to the user this round, in the answer-prompt shape.
pub fn frontier_markdown(plan: &Plan, ready: &[String]) -> Result<String> {
    plan.require_supported_schema()?;
    let mut md = Md::default();
    for id in ready {
        // `ready` comes from the frontier derivation, which already decided what may be asked.
        // Re-deriving it here would mean two places could disagree about the same round.
        let Some(decision) = plan.decision(id) else {
            continue;
        };
        md.blank();
        md.line(format!(
            "{}. {}",
            inline(&decision.id),
            inline(&decision.question)
        ));
        if !decision.alternatives.is_empty() {
            md.blank();
            for alt in sorted_by_id(&decision.alternatives, |a| a.id.as_str()) {
                md.line(format!("{}. {}", inline(&alt.id), inline(&alt.value)));
            }
        }
        md.blank();
        render_compact_recommendation(&decision.recommendation, &mut md);
    }
    md.blank();
    md.line("Answer forms:");
    md.line("- C<n>=REC — take the recommendation as displayed");
    md.line("- C<n>=ALT<m> — take that alternative");
    md.line("- C<n>=OTHER: <value> — answer in your own words");
    md.line("- C<n>=UNKNOWN — record it as an open item");
    md.line("- S<n>=NA: <reason> — close a surface as not applicable");
    Ok(md.finish())
}

fn render_header(plan: &Plan, md: &mut Md) -> Result<()> {
    let digest = plan.digest()?;
    md.line(format!("# {}", inline(&plan.goal.statement)));
    md.blank();
    md.line(format!("- Goal id: {}", inline(&plan.goal_id)));
    md.line(format!("- Revision: {}", plan.revision));
    md.line(format!("- Base branch: {}", inline(&plan.base_branch)));
    md.line(format!("- Plan digest: {digest}"));
    md.line(format!("- Challenge: {}", digest.challenge()));
    Ok(())
}

fn render_goal(plan: &Plan, md: &mut Md) {
    md.blank();
    md.line("## Goal");
    md.blank();
    md.line(inline(&plan.goal.statement));
    md.prose_list("Success:", &plan.goal.success);
    md.prose_list("Non-goals:", &plan.goal.non_goals);
}

fn render_surfaces(plan: &Plan, md: &mut Md) -> Result<()> {
    for id in plan.surfaces.keys() {
        if Surface::parse(id).is_none() {
            return Err(Error::Corrupt(format!(
                "plan.surfaces holds {id:?}, which is not one of S1..S12"
            )));
        }
    }
    md.blank();
    md.line("## Surfaces");
    md.blank();
    md.table_header(&["Surface", "Title", "Status", "Reason"]);
    for surface in SURFACES {
        let status = plan.surfaces.get(surface.id()).ok_or_else(|| {
            // Rendering eleven rows for a twelve-surface checklist would read as "S7 was handled".
            Error::Corrupt(format!(
                "plan.surfaces is missing {surface}; a surface cannot be closed by omission"
            ))
        })?;
        let (state, reason) = match status {
            SurfaceStatus::Applicable => ("applicable", ""),
            SurfaceStatus::NotApplicable { reason, .. } => ("not applicable", reason.as_str()),
        };
        md.row(&[surface.id(), surface.title(), state, reason]);
    }
    Ok(())
}

fn render_facts(plan: &Plan, md: &mut Md) {
    if plan.facts.is_empty() {
        return;
    }
    md.blank();
    md.line("## Facts");
    md.blank();
    md.table_header(&["Id", "Question", "Answer", "Sources"]);
    for fact in sorted_by_id(&plan.facts, |f| f.id.as_str()) {
        let sources = quoted_join(&fact.sources);
        md.row(&[&fact.id, &fact.question, &fact.answer, &sources]);
    }
}

fn render_decisions(plan: &Plan, md: &mut Md) -> Result<()> {
    if plan.decisions.is_empty() {
        return Ok(());
    }
    md.blank();
    md.line("## Decisions");
    for decision in sorted_by_id(&plan.decisions, |d| d.id.as_str()) {
        md.blank();
        md.line(format!(
            "### {} · {} · {}",
            inline(&decision.id),
            decision.surface,
            kind_label(decision.kind)
        ));
        md.blank();
        md.line(inline(&decision.question));
        if !decision.alternatives.is_empty() {
            md.blank();
            for alt in sorted_by_id(&decision.alternatives, |a| a.id.as_str()) {
                md.line(format!("- {} — {}", inline(&alt.id), inline(&alt.value)));
            }
        }
        render_recommendation(&decision.recommendation, md);
        md.blank();
        md.line(answer_line(decision)?);
    }
    Ok(())
}

fn render_recommendation(recommendation: &Recommendation, md: &mut Md) {
    md.blank();
    match recommendation {
        Recommendation::Recommended {
            choice,
            rationale,
            evidence,
            tradeoffs,
            impact,
            confidence,
        } => {
            md.line(format!("Recommendation: {}", inline(choice)));
            md.prose_list("Rationale:", rationale);
            md.id_list("Evidence:", evidence);
            md.prose_list("Trade-offs:", tradeoffs);
            md.id_list("Impact:", impact);
            md.blank();
            md.line(format!("Confidence: {}", confidence_label(*confidence)));
        }
        Recommendation::NoRecommendation { rationale } => {
            md.line("Recommendation: none");
            md.prose_list("Rationale:", rationale);
        }
        Recommendation::ProbeRequired {
            probe_unit,
            rationale,
        } => {
            md.line(format!(
                "Recommendation: probe {} first",
                inline(probe_unit)
            ));
            md.prose_list("Rationale:", rationale);
        }
    }
}

/// The one-line-per-label form used in the answer prompt, where the whole frontier has to stay
/// readable in a chat message rather than in a document.
fn render_compact_recommendation(recommendation: &Recommendation, md: &mut Md) {
    match recommendation {
        Recommendation::Recommended {
            choice,
            rationale,
            evidence,
            tradeoffs,
            impact,
            confidence,
        } => {
            md.line(format!("Recommendation: {}", inline(choice)));
            md.prose_line("Rationale:", rationale);
            md.id_line("Evidence:", evidence);
            md.prose_line("Trade-offs:", tradeoffs);
            md.id_line("Impact:", impact);
            md.line(format!("Confidence: {}", confidence_label(*confidence)));
        }
        Recommendation::NoRecommendation { rationale } => {
            md.line("Recommendation: none");
            md.prose_line("Rationale:", rationale);
        }
        Recommendation::ProbeRequired {
            probe_unit,
            rationale,
        } => {
            md.line(format!(
                "Recommendation: probe {} first",
                inline(probe_unit)
            ));
            md.prose_line("Rationale:", rationale);
        }
    }
}

fn answer_line(decision: &Decision) -> Result<String> {
    let Some(answer) = &decision.answer else {
        return Ok("Answer: (unanswered)".to_string());
    };
    Ok(match decision.answer_freshness()? {
        AnswerFreshness::Fresh => match decision.resolved_value()? {
            Some(value) => format!(
                "Answer: {} (resolved: {})",
                inline(&answer.text),
                inline(&value)
            ),
            // `UNKNOWN` and a dangling `ALT<m>` are both answered and both resolve to nothing;
            // printing the user's words with no outcome would read as a resolution.
            None => format!("Answer: {} (no resolved value)", inline(&answer.text)),
        },
        stale => format!("Answer: {}", stale.explain(&decision.id)),
    })
}

fn render_requirements(plan: &Plan, md: &mut Md) {
    if plan.requirements.is_empty() {
        return;
    }
    md.blank();
    md.line("## Requirements");
    md.blank();
    md.table_header(&["Id", "Statement", "Decisions"]);
    for requirement in sorted_by_id(&plan.requirements, |r| r.id.as_str()) {
        let decisions = sorted_join(&requirement.decision_ids);
        md.row(&[&requirement.id, &requirement.statement, &decisions]);
    }
}

fn render_acceptance(plan: &Plan, md: &mut Md) {
    if plan.acceptance.is_empty() {
        return;
    }
    md.blank();
    md.line("## Acceptance");
    md.blank();
    md.table_header(&["Id", "Observable", "Requirements"]);
    for acceptance in sorted_by_id(&plan.acceptance, |a| a.id.as_str()) {
        let requirements = sorted_join(&acceptance.requirement_ids);
        md.row(&[&acceptance.id, &acceptance.observable, &requirements]);
    }
}

fn render_units(plan: &Plan, md: &mut Md) {
    if plan.units.is_empty() {
        return;
    }
    md.blank();
    md.line("## Units");
    md.blank();
    md.table_header(&["Id", "Title", "Probe", "Acceptance", "Depends on", "Paths"]);
    for unit in unit_order(plan).0 {
        let acceptance = sorted_join(&unit.acceptance_ids);
        let depends_on = sorted_join(&unit.depends_on);
        let paths = quoted_join(&unit.paths);
        md.row(&[
            &unit.id,
            &unit.title,
            if unit.probe { "yes" } else { "no" },
            &acceptance,
            &depends_on,
            &paths,
        ]);
    }
}

fn render_tests(plan: &Plan, md: &mut Md) {
    if plan.tests.is_empty() {
        return;
    }
    md.blank();
    md.line("## Tests");
    md.blank();
    md.table_header(&["Id", "Command", "Unit", "Acceptance"]);
    for test in sorted_by_id(&plan.tests, |t| t.id.as_str()) {
        let acceptance = sorted_join(&test.acceptance_ids);
        md.row(&[&test.id, &test.command, &test.unit_id, &acceptance]);
    }
}

fn render_open_items(plan: &Plan, md: &mut Md) {
    if plan.open_items.is_empty() {
        return;
    }
    md.blank();
    md.line("## Open items");
    md.blank();
    md.table_header(&["Id", "Decision", "Detail"]);
    for item in sorted_by_id(&plan.open_items, |i| i.id.as_str()) {
        md.row(&[&item.id, &item.decision_id, &item.detail]);
    }
}

fn render_reviews(plan: &Plan, md: &mut Md) -> Result<()> {
    let reviewed = plan.review_digest()?;
    md.blank();
    md.line("## Reviews");
    render_review(
        "Cold consumer",
        plan.reviews.cold_consumer.as_ref(),
        &reviewed,
        md,
    );
    render_review("Critic", plan.reviews.critic.as_ref(), &reviewed, md);
    Ok(())
}

fn render_review(label: &str, review: Option<&PlanReview>, reviewed: &Digest, md: &mut Md) {
    md.blank();
    let Some(review) = review else {
        md.line(format!("{label}: absent"));
        return;
    };
    md.line(format!(
        "{label}: present, {}, {}",
        if review.passed { "passed" } else { "failed" },
        if review.plan_digest == *reviewed {
            "fresh"
        } else {
            "stale"
        }
    ));
    for finding in &review.findings {
        md.line(format!("- {}", inline(finding)));
    }
}

fn render_confirm(plan: &Plan, md: &mut Md) -> Result<()> {
    let blockers = validate::freeze_blockers(plan)?;
    md.blank();
    md.line("## Confirm");
    md.blank();
    if blockers.is_empty() {
        md.line("Type this line exactly:");
        md.blank();
        md.line("```");
        md.line(format!("CONFIRM PLAN {}", plan.challenge()?));
        md.line("```");
    } else {
        md.line("The plan cannot be frozen yet:");
        for violation in blockers {
            // The code is the stable half, so a host can act on it while the wording changes.
            md.line(format!(
                "- {}: {}",
                violation.code,
                inline(&violation.detail)
            ));
        }
    }
    Ok(())
}

/// The units in dependency order, and whether that order is a real topological one.
///
/// Ties are broken by id, so the result depends on the graph and the ids alone. A cycle returns the
/// units in plan order with `false`: the reader needs to see every unit in order to find the cycle,
/// and the order they wrote them in is the one they can navigate.
fn unit_order(plan: &Plan) -> (Vec<&Unit>, bool) {
    let count = plan.units.len();
    let mut by_id: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, unit) in plan.units.iter().enumerate() {
        by_id.entry(unit.id.as_str()).or_default().push(index);
    }
    let mut pending: Vec<usize> = (0..count).collect();
    pending.sort_by(|&a, &b| id_key(&plan.units[a].id).cmp(&id_key(&plan.units[b].id)));

    let mut emitted = vec![false; count];
    let mut order: Vec<&Unit> = Vec::with_capacity(count);
    while order.len() < count {
        let next = pending.iter().position(|&index| {
            plan.units[index].depends_on.iter().all(|dependency| {
                // A dependency on a unit the plan never defines can never be met. Ordering treats it
                // as met so that one dangling edge does not push every other unit into the cycle
                // fallback; the dangling edge itself is a validation finding, not a layout problem.
                by_id
                    .get(dependency.as_str())
                    .is_none_or(|indices| indices.iter().all(|&i| emitted[i]))
            })
        });
        let Some(slot) = next else {
            return (plan.units.iter().collect(), false);
        };
        let index = pending.remove(slot);
        emitted[index] = true;
        order.push(&plan.units[index]);
    }
    (order, true)
}

fn kind_label(kind: DecisionKind) -> &'static str {
    match kind {
        DecisionKind::Decision => "decision",
        DecisionKind::Scenario => "scenario",
        DecisionKind::Term => "term",
    }
}

fn confidence_label(confidence: Confidence) -> &'static str {
    match confidence {
        Confidence::Low => "low",
        Confidence::Medium => "medium",
        Confidence::High => "high",
    }
}

/// Orders `<prefix><n>` identifiers numerically, so `C2` precedes `C10`.
///
/// An id without a numeric suffix sorts after every numbered one and then by the id itself: a
/// malformed id must still land somewhere fixed, because dropping it or ordering it by chance would
/// hide it from the reader who has to fix it.
fn id_key(id: &str) -> (u64, &str) {
    let digits = id.len() - id.bytes().rev().take_while(u8::is_ascii_digit).count();
    // `digits` is preceded only by ASCII digits, which are never UTF-8 continuation bytes, so it is
    // always a char boundary and the slice below cannot panic.
    (id[digits..].parse::<u64>().unwrap_or(u64::MAX), id)
}

fn sorted_by_id<'a, T, F>(items: &'a [T], id: F) -> Vec<&'a T>
where
    F: Fn(&'a T) -> &'a str,
{
    let mut sorted: Vec<&T> = items.iter().collect();
    sorted.sort_by(|a, b| id_key(id(a)).cmp(&id_key(id(b))));
    sorted
}

fn sorted_ids(ids: &[String]) -> Vec<&str> {
    let mut sorted: Vec<&str> = ids.iter().map(String::as_str).collect();
    sorted.sort_by(|a, b| id_key(a).cmp(&id_key(b)));
    sorted
}

fn sorted_join(ids: &[String]) -> String {
    sorted_ids(ids).join(", ")
}

/// Joins values that are not identifiers, each one in backticks.
///
/// A comma is content in a path or a source location, so joining them the way identifiers are joined
/// would render the single path `docs, src` and the two paths `docs` and `src` as the same cell —
/// and the Paths cell is the only place the document says what a unit is allowed to rewrite.
/// [`crate::validate`] refuses a path carrying either delimiter, so for units the cell is unambiguous
/// by the time it can be frozen.
fn quoted_join(values: &[String]) -> String {
    sorted_ids(values)
        .into_iter()
        .map(|value| format!("`{value}`"))
        .collect::<Vec<String>>()
        .join(", ")
}

/// Flattens a value so it can occupy one line of the document.
///
/// Control characters become spaces, which is what stops a goal statement containing `\n## Confirm`
/// from forging a section the plan does not have.
fn inline(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// Flattens a value so it can occupy one cell of a table.
///
/// The backslash is escaped before the pipe: without that, a value the author wrote as the literal
/// text `a\|b` would arrive as `a\\|b` and end the cell at a pipe that is not a column break.
fn cell(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '|' => out.push_str("\\|"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// A markdown document under construction.
///
/// Every line goes through [`Md::line`], which is what makes "no trailing whitespace" and "exactly
/// one blank line between blocks" invariants of the type rather than promises each caller has to
/// keep.
#[derive(Default)]
struct Md {
    lines: Vec<String>,
}

impl Md {
    fn line(&mut self, text: impl AsRef<str>) {
        let text = text.as_ref().trim_end();
        if text.is_empty() {
            // A blank line is a separator, never content, so runs of them collapse and a leading one
            // is dropped. Callers can then say "new block" without knowing what came before.
            if self.lines.last().is_some_and(|last| !last.is_empty()) {
                self.lines.push(String::new());
            }
            return;
        }
        self.lines.push(text.to_string());
    }

    fn blank(&mut self) {
        self.line("");
    }

    fn row(&mut self, cells: &[&str]) {
        let mut line = String::from("|");
        for text in cells {
            line.push(' ');
            line.push_str(&cell(text));
            line.push_str(" |");
        }
        self.line(line);
    }

    fn table_header(&mut self, cells: &[&str]) {
        self.row(cells);
        let rule: Vec<&str> = cells.iter().map(|_| "---").collect();
        self.row(&rule);
    }

    /// A labelled bullet list in the author's order, omitted whole when empty rather than printing a
    /// heading over nothing.
    fn prose_list(&mut self, label: &str, items: &[String]) {
        if items.is_empty() {
            return;
        }
        self.blank();
        self.line(label);
        for item in items {
            self.line(format!("- {}", inline(item)));
        }
    }

    /// A labelled bullet list of identifiers, sorted because their order carries no meaning.
    fn id_list(&mut self, label: &str, ids: &[String]) {
        if ids.is_empty() {
            return;
        }
        self.blank();
        self.line(label);
        for id in sorted_ids(ids) {
            self.line(format!("- {}", inline(id)));
        }
    }

    fn prose_line(&mut self, label: &str, items: &[String]) {
        if items.is_empty() {
            return;
        }
        let joined: Vec<String> = items.iter().map(|item| inline(item)).collect();
        // Semicolons, not commas: these are sentences, and sentences contain commas.
        self.line(format!("{label} {}", joined.join("; ")));
    }

    fn id_line(&mut self, label: &str, ids: &[String]) {
        if ids.is_empty() {
            return;
        }
        self.line(format!("{label} {}", sorted_join(ids)));
    }

    fn finish(mut self) -> String {
        while self.lines.last().is_some_and(String::is_empty) {
            self.lines.pop();
        }
        let mut out = self.lines.join("\n");
        out.push('\n');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::{Clock, FixedClock};
    use crate::plan::{
        Acceptance, Alternative, Answer, Fact, OpenItem, PlanReviews, Requirement, Selection, Test,
    };

    fn ts() -> String {
        FixedClock::new("2026-09-04T00:00:00Z").now()
    }

    fn recommended() -> Recommendation {
        Recommendation::Recommended {
            choice: "ALT1".into(),
            rationale: vec!["keeps validation parity with apply".into()],
            evidence: vec!["F7".into()],
            tradeoffs: vec!["a webhook failure becomes a dry-run failure".into()],
            impact: vec!["tests".into(), "api".into()],
            confidence: Confidence::High,
        }
    }

    fn decision(id: &str) -> Decision {
        Decision {
            id: id.into(),
            surface: Surface::S2,
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
            recommendation: recommended(),
            depends_on: vec![],
            answer: None,
        }
    }

    fn answer(decision: &Decision, selection: Selection, text: &str) -> Answer {
        Answer {
            text: text.into(),
            recommendation: matches!(selection, Selection::Recommendation)
                .then(|| decision.recommendation_digest().unwrap()),
            selection,
            ts: ts(),
            identity: decision.identity_digest().unwrap(),
        }
    }

    fn answer_it(decision: &mut Decision, selection: Selection, text: &str) {
        decision.answer = Some(answer(decision, selection, text));
    }

    fn unit(id: &str, depends_on: &[&str]) -> Unit {
        Unit {
            id: id.into(),
            title: format!("build {id}"),
            paths: vec!["src/".into()],
            acceptance_ids: vec!["A1".into()],
            depends_on: depends_on.iter().map(|d| (*d).to_string()).collect(),
            probe: false,
        }
    }

    /// Closes a surface the way a user closes one, with `S<n>=NA`.
    fn close(plan: &mut Plan, surface: Surface) {
        plan.surfaces.insert(
            surface.id().into(),
            SurfaceStatus::NotApplicable {
                reason: format!("{surface} does not apply to a dry-run flag"),
                answer: Answer {
                    text: format!("{surface}=NA"),
                    selection: Selection::NotApplicable,
                    ts: ts(),
                    identity: Digest::zero(),
                    recommendation: None,
                },
            },
        );
    }

    /// A plan `validate::freeze_blockers` accepts, so a test can take away exactly one thing.
    ///
    /// Only S2 stays open. The gate wants an answered decision — and an answered scenario — on every
    /// surface the user left applicable, and twelve surfaces' worth of decisions would bury the
    /// layout the rest of these tests are about.
    fn freezable_plan() -> Plan {
        let mut plan = Plan::new("2026-09-04-dry-run", "main", "add a dry-run mode to apply");
        plan.goal.success = vec!["apply --dry-run prints the diff".into()];
        plan.goal.non_goals = vec!["no server-side dry-run".into()];
        for surface in SURFACES.into_iter().filter(|s| *s != Surface::S2) {
            close(&mut plan, surface);
        }
        plan.facts.push(Fact {
            id: "F7".into(),
            question: "does apply call the webhook?".into(),
            answer: "yes".into(),
            sources: vec!["src/apply/mod.rs:120-146".into()],
        });
        let mut first = decision("C1");
        answer_it(&mut first, Selection::Recommendation, "C1=REC");
        let mut second = decision("C2");
        second.kind = DecisionKind::Scenario;
        second.question = "What must happen when the webhook times out?".into();
        answer_it(
            &mut second,
            Selection::Alternative { id: "ALT2".into() },
            "C2=ALT2",
        );
        plan.decisions = vec![first, second];
        plan.requirements = vec![
            Requirement {
                id: "R1".into(),
                statement: "dry-run calls the webhook".into(),
                decision_ids: vec!["C1".into()],
            },
            Requirement {
                id: "R2".into(),
                statement: "a webhook timeout fails the dry-run".into(),
                decision_ids: vec!["C2".into()],
            },
        ];
        plan.acceptance = vec![
            Acceptance {
                id: "A1".into(),
                requirement_ids: vec!["R1".into()],
                observable: "the webhook receives a dry-run request".into(),
            },
            Acceptance {
                id: "A2".into(),
                requirement_ids: vec!["R2".into()],
                observable: "a timed-out webhook exits non-zero".into(),
            },
        ];
        let mut covers_a2 = unit("U2", &["U1"]);
        covers_a2.acceptance_ids = vec!["A2".into()];
        plan.units = vec![unit("U1", &[]), covers_a2];
        plan.tests = vec![
            Test {
                id: "T1".into(),
                command: "cargo test webhook".into(),
                acceptance_ids: vec!["A1".into()],
                unit_id: "U1".into(),
            },
            Test {
                id: "T2".into(),
                command: "cargo test timeout".into(),
                acceptance_ids: vec!["A2".into()],
                unit_id: "U2".into(),
            },
        ];
        plan.full_suite = "cargo test".into();
        pass_reviews(&mut plan);
        plan
    }

    /// Records two fresh passing reviews. Must run last: a review binds to the plan it read.
    fn pass_reviews(plan: &mut Plan) {
        let digest = plan.review_digest().unwrap();
        let review = PlanReview {
            plan_digest: digest,
            ts: ts(),
            passed: true,
            findings: vec![],
        };
        plan.reviews = PlanReviews {
            cold_consumer: Some(review.clone()),
            critic: Some(review),
        };
    }

    fn section<'a>(document: &'a str, heading: &str) -> &'a str {
        let start = document
            .find(heading)
            .unwrap_or_else(|| panic!("{heading} is missing from:\n{document}"));
        let rest = &document[start..];
        match rest[heading.len()..].find("\n## ") {
            Some(end) => &rest[..heading.len() + end],
            None => rest,
        }
    }

    #[test]
    fn the_document_opens_with_the_goal_statement_and_its_metadata() {
        let plan = freezable_plan();
        let rendered = plan_markdown(&plan).unwrap();
        let digest = plan.digest().unwrap();
        let head: Vec<&str> = rendered.lines().take(7).collect();
        assert_eq!(
            head,
            vec![
                "# add a dry-run mode to apply",
                "",
                "- Goal id: 2026-09-04-dry-run",
                "- Revision: 1",
                "- Base branch: main",
                &format!("- Plan digest: {digest}"),
                &format!("- Challenge: {}", digest.challenge()),
            ]
        );
    }

    #[test]
    fn the_metadata_digest_is_the_one_the_challenge_is_derived_from() {
        let mut plan = freezable_plan();
        plan.revision = 9;
        let rendered = plan_markdown(&plan).unwrap();
        let digest = plan.digest().unwrap();
        assert!(
            rendered.contains(&format!("- Plan digest: {digest}")),
            "{rendered}"
        );
        assert!(digest.as_str().starts_with("sha256:"));
        assert!(
            rendered.contains(&format!("- Challenge: {}", plan.challenge().unwrap())),
            "{rendered}"
        );
    }

    #[test]
    fn rendering_the_same_plan_twice_is_byte_identical() {
        let plan = freezable_plan();
        assert_eq!(plan_markdown(&plan).unwrap(), plan_markdown(&plan).unwrap());
    }

    /// The document minus the two lines that quote the digest, which is order-sensitive on purpose.
    fn body(document: &str) -> String {
        document
            .lines()
            .filter(|line| {
                !line.starts_with("- Plan digest: ") && !line.starts_with("- Challenge: ")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn shuffling_identifier_lists_does_not_change_a_single_byte_of_the_body() {
        let mut plan = freezable_plan();
        plan.decisions.push(decision("C3"));
        plan.facts.push(Fact {
            id: "F2".into(),
            question: "second".into(),
            answer: "yes".into(),
            sources: vec!["a.rs:1".into(), "b.rs:2".into()],
        });
        let mut extra = unit("U3", &["U1"]);
        extra.paths = vec!["tests/".into(), "src/".into()];
        extra.acceptance_ids = vec!["A3".into(), "A1".into()];
        plan.units.push(extra);
        plan.acceptance.push(Acceptance {
            id: "A3".into(),
            requirement_ids: vec!["R1".into()],
            observable: "the diff is printed".into(),
        });
        plan.tests.push(Test {
            id: "T3".into(),
            command: "cargo test diff".into(),
            acceptance_ids: vec!["A3".into(), "A1".into()],
            unit_id: "U3".into(),
        });
        // A review binds to the plan's canonical JSON, in which array order is content, so a shuffled
        // plan is a different plan to a reviewer. Reviews are left out to keep this test about layout.
        plan.reviews = PlanReviews::default();
        let straight = plan_markdown(&plan).unwrap();

        let mut shuffled = plan.clone();
        shuffled.decisions.reverse();
        shuffled.facts.reverse();
        shuffled.units.reverse();
        shuffled.acceptance.reverse();
        shuffled.tests.reverse();
        shuffled.decisions[0].alternatives.reverse();
        for unit in &mut shuffled.units {
            unit.paths.reverse();
            unit.acceptance_ids.reverse();
        }
        for test in &mut shuffled.tests {
            test.acceptance_ids.reverse();
        }
        for fact in &mut shuffled.facts {
            fact.sources.reverse();
        }
        assert_eq!(body(&plan_markdown(&shuffled).unwrap()), body(&straight));
        assert_ne!(
            shuffled.digest().unwrap(),
            plan.digest().unwrap(),
            "array order is content to the digest, and the header must quote the real digest"
        );
    }

    #[test]
    fn no_rendered_line_carries_trailing_whitespace() {
        let mut plan = freezable_plan();
        plan.goal.statement = "trailing spaces follow   ".into();
        plan.goal.success = vec!["so does this\t".into()];
        plan.facts[0].answer = "and this ".into();
        for document in [
            plan_markdown(&plan).unwrap(),
            frontier_markdown(&plan, &["C1".into()]).unwrap(),
        ] {
            for line in document.lines() {
                assert_eq!(line, line.trim_end(), "trailing whitespace in {line:?}");
            }
        }
    }

    #[test]
    fn every_document_ends_with_exactly_one_newline() {
        let plan = freezable_plan();
        for document in [
            plan_markdown(&plan).unwrap(),
            frontier_markdown(&plan, &["C1".into()]).unwrap(),
        ] {
            assert!(document.ends_with('\n'), "{document}");
            assert!(!document.ends_with("\n\n"), "{document}");
        }
    }

    #[test]
    fn blocks_are_separated_by_exactly_one_blank_line() {
        let mut plan = freezable_plan();
        // An empty goal statement is the case that used to leave two blank lines behind it.
        plan.goal.statement = String::new();
        for document in [
            plan_markdown(&plan).unwrap(),
            frontier_markdown(&plan, &["C1".into()]).unwrap(),
        ] {
            assert!(
                !document.contains("\n\n\n"),
                "double blank line in:\n{document}"
            );
            assert!(!document.starts_with('\n'), "{document}");
        }
    }

    #[test]
    fn an_empty_success_or_non_goal_list_omits_its_heading() {
        let mut plan = freezable_plan();
        plan.goal.non_goals.clear();
        let rendered = plan_markdown(&plan).unwrap();
        assert!(
            rendered.contains("Success:\n- apply --dry-run prints the diff"),
            "{rendered}"
        );
        assert!(!rendered.contains("Non-goals:"), "{rendered}");
    }

    #[test]
    fn all_twelve_surfaces_are_listed_in_order_with_their_titles() {
        let plan = freezable_plan();
        let rendered = plan_markdown(&plan).unwrap();
        let surfaces = section(&rendered, "## Surfaces");
        let rows: Vec<&str> = surfaces.lines().skip(4).collect();
        assert_eq!(rows.len(), 12, "{surfaces}");
        for (row, surface) in rows.iter().zip(SURFACES) {
            let expected = if surface == Surface::S2 {
                format!("| {} | {} | applicable |  |", surface.id(), surface.title())
            } else {
                format!(
                    "| {} | {} | not applicable | {surface} does not apply to a dry-run flag |",
                    surface.id(),
                    surface.title()
                )
            };
            assert_eq!(*row, expected);
        }
    }

    #[test]
    fn a_surface_closed_as_not_applicable_shows_the_users_reason() {
        let mut plan = freezable_plan();
        plan.surfaces.insert(
            "S9".into(),
            SurfaceStatus::NotApplicable {
                reason: "no released consumers".into(),
                answer: Answer {
                    text: "S9=NA: no released consumers".into(),
                    selection: Selection::NotApplicable,
                    ts: ts(),
                    identity: Digest::zero(),
                    recommendation: None,
                },
            },
        );
        let rendered = plan_markdown(&plan).unwrap();
        assert!(
            rendered.contains(&format!(
                "| S9 | {} | not applicable | no released consumers |",
                Surface::S9.title()
            )),
            "{rendered}"
        );
    }

    #[test]
    fn a_missing_surface_is_corrupt_rather_than_a_shorter_table() {
        let mut plan = freezable_plan();
        plan.surfaces.remove("S7");
        let err = plan_markdown(&plan).unwrap_err();
        assert!(matches!(err, Error::Corrupt(_)), "{err:?}");
        let message = err.to_string();
        assert!(message.contains("missing S7"), "{message}");
        assert!(message.contains("closed by omission"), "{message}");
    }

    #[test]
    fn a_surface_id_outside_s1_to_s12_is_corrupt() {
        for bad in ["S13", "s1", "", "S1 "] {
            let mut plan = freezable_plan();
            plan.surfaces.insert(bad.into(), SurfaceStatus::Applicable);
            let err = plan_markdown(&plan).unwrap_err();
            assert!(matches!(err, Error::Corrupt(_)), "{bad:?} gave {err:?}");
            assert!(
                err.to_string().contains(&format!("{bad:?}")),
                "{bad:?} gave {err}"
            );
        }
    }

    #[test]
    fn a_plan_from_another_schema_is_rejected_before_any_of_it_is_rendered() {
        let mut plan = freezable_plan();
        plan.schema = "hwahap/v2".into();
        for err in [
            plan_markdown(&plan).unwrap_err(),
            frontier_markdown(&plan, &["C1".into()]).unwrap_err(),
        ] {
            assert!(matches!(err, Error::Rejected(_)), "{err:?}");
            assert!(err.to_string().contains("hwahap/v2"), "{err}");
        }
    }

    #[test]
    fn facts_carry_their_sources_as_a_backtick_quoted_list() {
        let mut plan = freezable_plan();
        plan.facts[0].sources = vec!["src/b.rs:2".into(), "src/a.rs:1".into()];
        let rendered = plan_markdown(&plan).unwrap();
        assert!(
            rendered.contains(
                "| F7 | does apply call the webhook? | yes | `src/a.rs:1`, `src/b.rs:2` |"
            ),
            "{rendered}"
        );
    }

    #[test]
    fn the_facts_section_is_omitted_when_the_plan_states_none() {
        let mut plan = freezable_plan();
        plan.facts.clear();
        let rendered = plan_markdown(&plan).unwrap();
        assert!(!rendered.contains("## Facts"), "{rendered}");
    }

    #[test]
    fn identifiers_are_ordered_numerically_not_lexicographically() {
        let mut plan = freezable_plan();
        for id in ["C10", "C3"] {
            let mut extra = decision(id);
            answer_it(
                &mut extra,
                Selection::Alternative { id: "ALT2".into() },
                "x",
            );
            plan.decisions.push(extra);
        }
        let rendered = plan_markdown(&plan).unwrap();
        let headings: Vec<&str> = rendered
            .lines()
            .filter(|line| line.starts_with("### "))
            .collect();
        assert_eq!(
            headings,
            vec![
                "### C1 · S2 · decision",
                "### C2 · S2 · scenario",
                "### C3 · S2 · decision",
                "### C10 · S2 · decision",
            ]
        );
    }

    #[test]
    fn an_identifier_without_a_number_sorts_last_and_is_still_shown() {
        let mut plan = freezable_plan();
        let mut odd = decision("Cx");
        answer_it(&mut odd, Selection::Unknown, "Cx=UNKNOWN");
        plan.decisions.push(odd);
        let rendered = plan_markdown(&plan).unwrap();
        let headings: Vec<&str> = rendered
            .lines()
            .filter(|line| line.starts_with("### "))
            .collect();
        assert_eq!(
            headings,
            vec![
                "### C1 · S2 · decision",
                "### C2 · S2 · scenario",
                "### Cx · S2 · decision",
            ]
        );
    }

    #[test]
    fn a_recommended_decision_renders_every_part_of_its_advice() {
        let plan = freezable_plan();
        let rendered = plan_markdown(&plan).unwrap();
        assert!(
            section(&rendered, "## Decisions").contains(concat!(
                "Recommendation: ALT1\n",
                "\n",
                "Rationale:\n",
                "- keeps validation parity with apply\n",
                "\n",
                "Evidence:\n",
                "- F7\n",
                "\n",
                "Trade-offs:\n",
                "- a webhook failure becomes a dry-run failure\n",
                "\n",
                "Impact:\n",
                "- api\n",
                "- tests\n",
                "\n",
                "Confidence: high\n",
            )),
            "{rendered}"
        );
    }

    #[test]
    fn a_decision_with_no_recommendation_says_none_and_shows_only_its_rationale() {
        let mut plan = freezable_plan();
        plan.decisions.truncate(1);
        plan.decisions[0].recommendation = Recommendation::NoRecommendation {
            rationale: vec!["both are defensible".into()],
        };
        plan.decisions[0].answer = None;
        let rendered = plan_markdown(&plan).unwrap();
        let decisions = section(&rendered, "## Decisions");
        assert!(
            decisions.contains("Recommendation: none\n\nRationale:\n- both are defensible\n"),
            "{decisions}"
        );
        assert!(!decisions.contains("Confidence:"), "{decisions}");
        assert!(!decisions.contains("Evidence:"), "{decisions}");
    }

    #[test]
    fn a_probe_required_decision_names_the_probe_unit() {
        let mut plan = freezable_plan();
        plan.decisions[0].recommendation = Recommendation::ProbeRequired {
            probe_unit: "U9".into(),
            rationale: vec!["measure the webhook latency first".into()],
        };
        plan.decisions[0].answer = None;
        let rendered = plan_markdown(&plan).unwrap();
        assert!(
            section(&rendered, "## Decisions").contains(
                "Recommendation: probe U9 first\n\nRationale:\n- measure the webhook latency first\n"
            ),
            "{rendered}"
        );
    }

    #[test]
    fn a_recommendation_with_no_rationale_omits_the_heading_instead_of_an_empty_bullet() {
        let mut plan = freezable_plan();
        plan.decisions.truncate(1);
        plan.decisions[0].recommendation = Recommendation::NoRecommendation { rationale: vec![] };
        plan.decisions[0].answer = None;
        let rendered = plan_markdown(&plan).unwrap();
        let decisions = section(&rendered, "## Decisions");
        assert!(decisions.contains("Recommendation: none\n"), "{decisions}");
        assert!(!decisions.contains("Rationale:"), "{decisions}");
        assert!(!decisions.contains("- \n"), "{decisions}");
    }

    #[test]
    fn an_unanswered_decision_renders_the_unanswered_marker() {
        let mut plan = freezable_plan();
        plan.decisions[0].answer = None;
        let rendered = plan_markdown(&plan).unwrap();
        assert!(rendered.contains("\nAnswer: (unanswered)\n"), "{rendered}");
    }

    #[test]
    fn a_fresh_answer_shows_the_users_own_words_and_what_they_resolve_to() {
        let mut plan = freezable_plan();
        answer_it(
            &mut plan.decisions[0],
            Selection::Other {
                value: "call it, but time it out at 2s".into(),
            },
            "C1=OTHER: call it, but time it out at 2s",
        );
        let rendered = plan_markdown(&plan).unwrap();
        assert!(
            rendered.contains(
                "\nAnswer: C1=OTHER: call it, but time it out at 2s \
                 (resolved: call it, but time it out at 2s)\n"
            ),
            "{rendered}"
        );
    }

    #[test]
    fn a_fresh_answer_that_resolves_to_nothing_says_so_rather_than_trailing_off() {
        let mut plan = freezable_plan();
        answer_it(&mut plan.decisions[0], Selection::Unknown, "C1=UNKNOWN");
        let rendered = plan_markdown(&plan).unwrap();
        assert!(
            rendered.contains("\nAnswer: C1=UNKNOWN (no resolved value)\n"),
            "{rendered}"
        );
    }

    #[test]
    fn a_reworded_question_renders_the_explanation_instead_of_the_stale_answer() {
        let mut plan = freezable_plan();
        plan.decisions[0].question = "Call the admission webhook during dry-run, ever?".into();
        let rendered = plan_markdown(&plan).unwrap();
        assert!(
            rendered.contains(&format!(
                "\nAnswer: {}\n",
                AnswerFreshness::StaleQuestion.explain("C1")
            )),
            "{rendered}"
        );
        assert!(!rendered.contains("Answer: C1=REC"), "{rendered}");
    }

    #[test]
    fn a_changed_recommendation_renders_the_stale_recommendation_explanation() {
        let mut plan = freezable_plan();
        plan.decisions[0].recommendation = Recommendation::Recommended {
            choice: "ALT2".into(),
            rationale: vec!["latency dominates".into()],
            evidence: vec![],
            tradeoffs: vec![],
            impact: vec![],
            confidence: Confidence::Low,
        };
        let rendered = plan_markdown(&plan).unwrap();
        assert!(
            rendered.contains(&format!(
                "\nAnswer: {}\n",
                AnswerFreshness::StaleRecommendation.explain("C1")
            )),
            "{rendered}"
        );
    }

    #[test]
    fn alternatives_are_listed_by_id_with_their_values() {
        let mut plan = freezable_plan();
        plan.decisions[0].alternatives.reverse();
        let rendered = plan_markdown(&plan).unwrap();
        assert!(
            rendered.contains("- ALT1 — call it\n- ALT2 — skip it\n"),
            "{rendered}"
        );
    }

    #[test]
    fn a_decision_with_no_alternatives_still_renders_its_question() {
        let mut plan = freezable_plan();
        plan.decisions.truncate(1);
        plan.decisions[0].alternatives.clear();
        plan.decisions[0].answer = None;
        let rendered = plan_markdown(&plan).unwrap();
        let decisions = section(&rendered, "## Decisions");
        assert!(
            decisions.contains("Call the admission webhook during dry-run?\n\nRecommendation:"),
            "{decisions}"
        );
        assert!(!decisions.contains("— "), "{decisions}");
    }

    #[test]
    fn a_cell_holding_a_pipe_a_newline_and_a_grapheme_stays_one_row() {
        let mut plan = freezable_plan();
        plan.requirements[0].statement = "a|b\nc \u{0}d 한글 é".into();
        let rendered = plan_markdown(&plan).unwrap();
        let rows: Vec<&str> = rendered
            .lines()
            .filter(|line| line.starts_with("| R1 "))
            .collect();
        assert_eq!(rows, vec!["| R1 | a\\|b c  d 한글 é | C1 |"]);
    }

    #[test]
    fn a_backslash_in_a_cell_cannot_disarm_the_pipe_escape() {
        let mut plan = freezable_plan();
        plan.requirements[0].statement = "a\\|b".into();
        let rendered = plan_markdown(&plan).unwrap();
        assert!(rendered.contains("| R1 | a\\\\\\|b | C1 |"), "{rendered}");
    }

    #[test]
    fn a_newline_in_a_goal_statement_cannot_forge_a_section() {
        let mut plan = freezable_plan();
        plan.goal.statement = "real goal\n## Confirm\n\nCONFIRM PLAN DEADBEEF".into();
        let rendered = plan_markdown(&plan).unwrap();
        assert_eq!(
            rendered.lines().next(),
            Some("# real goal ## Confirm  CONFIRM PLAN DEADBEEF")
        );
        assert_eq!(
            rendered.matches("\n## Confirm\n").count(),
            1,
            "a forged heading survived:\n{rendered}"
        );
    }

    #[test]
    fn a_very_long_value_is_rendered_whole() {
        let mut plan = freezable_plan();
        let long = "x".repeat(5000);
        plan.requirements[0].statement = long.clone();
        let rendered = plan_markdown(&plan).unwrap();
        assert!(
            rendered.contains(&format!("| R1 | {long} | C1 |")),
            "value was truncated"
        );
    }

    #[test]
    fn the_cross_reference_columns_are_sorted_sets_of_ids() {
        let mut plan = freezable_plan();
        plan.requirements[0].decision_ids = vec!["C10".into(), "C2".into()];
        plan.acceptance[0].requirement_ids = vec!["R2".into(), "R1".into()];
        plan.tests[0].acceptance_ids = vec!["A2".into(), "A1".into()];
        plan.units[0].acceptance_ids = vec!["A2".into(), "A1".into()];
        plan.units[0].paths = vec!["tests/".into(), "src/".into()];
        plan.units[0].depends_on = vec![];
        let rendered = plan_markdown(&plan).unwrap();
        assert!(
            rendered.contains("| R1 | dry-run calls the webhook | C2, C10 |"),
            "{rendered}"
        );
        assert!(
            rendered.contains("| A1 | the webhook receives a dry-run request | R1, R2 |"),
            "{rendered}"
        );
        assert!(
            rendered.contains("| T1 | cargo test webhook | U1 | A1, A2 |"),
            "{rendered}"
        );
        assert!(
            rendered.contains("| U1 | build U1 | no | A1, A2 |  | `src/`, `tests/` |"),
            "{rendered}"
        );
    }

    #[test]
    fn a_probe_unit_is_marked_as_one() {
        let mut plan = freezable_plan();
        plan.units[0].probe = true;
        let rendered = plan_markdown(&plan).unwrap();
        assert!(
            rendered.contains("| U1 | build U1 | yes | A1 |  | `src/` |"),
            "{rendered}"
        );
    }

    #[test]
    fn units_are_listed_in_dependency_order() {
        let mut plan = freezable_plan();
        plan.units = vec![
            unit("U3", &["U2"]),
            unit("U1", &[]),
            unit("U2", &["U1"]),
            unit("U4", &[]),
        ];
        let rendered = plan_markdown(&plan).unwrap();
        let ids: Vec<&str> = rendered
            .lines()
            .filter(|line| line.starts_with("| U"))
            .map(|line| &line[2..4])
            .collect();
        assert_eq!(ids, vec!["U1", "U2", "U3", "U4"]);
    }

    #[test]
    fn a_cyclic_unit_graph_falls_back_to_plan_order_and_still_renders_every_unit() {
        let mut plan = freezable_plan();
        plan.units = vec![unit("U2", &["U1"]), unit("U1", &["U2"]), unit("U3", &[])];
        let rendered = plan_markdown(&plan).unwrap();
        let ids: Vec<&str> = rendered
            .lines()
            .filter(|line| line.starts_with("| U"))
            .map(|line| &line[2..4])
            .collect();
        assert_eq!(ids, vec!["U2", "U1", "U3"]);
        assert!(
            rendered.contains("- unit_cycle: unit dependencies cycle through U1, U2"),
            "{rendered}"
        );
    }

    #[test]
    fn a_unit_that_depends_on_itself_is_a_cycle() {
        let mut plan = freezable_plan();
        plan.units = vec![unit("U1", &["U1"])];
        let rendered = plan_markdown(&plan).unwrap();
        assert!(rendered.contains("- unit_cycle:"), "{rendered}");
        assert!(
            rendered.contains("| U1 | build U1 | no | A1 | U1 | `src/` |"),
            "{rendered}"
        );
    }

    #[test]
    fn a_dependency_on_a_unit_that_does_not_exist_does_not_disorder_the_rest() {
        let mut plan = freezable_plan();
        plan.units = vec![unit("U2", &["U1"]), unit("U1", &["U9"])];
        let rendered = plan_markdown(&plan).unwrap();
        let ids: Vec<&str> = rendered
            .lines()
            .filter(|line| line.starts_with("| U"))
            .map(|line| &line[2..4])
            .collect();
        assert_eq!(ids, vec!["U1", "U2"]);
        assert!(!rendered.contains("unit_cycle"), "{rendered}");
    }

    #[test]
    fn open_items_are_listed_and_the_section_disappears_when_there_are_none() {
        let mut plan = freezable_plan();
        assert!(!plan_markdown(&plan).unwrap().contains("## Open items"));
        plan.open_items.push(OpenItem {
            id: "O1".into(),
            decision_id: "C1".into(),
            detail: "needs a latency measurement".into(),
        });
        let rendered = plan_markdown(&plan).unwrap();
        assert!(
            rendered.contains("| O1 | C1 | needs a latency measurement |"),
            "{rendered}"
        );
        assert!(
            rendered.contains(
                "- open_item: open item O1 on C1 is unresolved: needs a latency measurement"
            ),
            "{rendered}"
        );
    }

    #[test]
    fn reviews_report_presence_outcome_freshness_and_findings() {
        let mut plan = freezable_plan();
        plan.reviews.cold_consumer = None;
        let stale = PlanReview {
            plan_digest: Digest::zero(),
            ts: ts(),
            passed: false,
            findings: vec!["A2 has no test".into(), "R3 is unfalsifiable".into()],
        };
        plan.reviews.critic = Some(stale);
        let rendered = plan_markdown(&plan).unwrap();
        assert_eq!(
            section(&rendered, "## Reviews"),
            concat!(
                "## Reviews\n",
                "\n",
                "Cold consumer: absent\n",
                "\n",
                "Critic: present, failed, stale\n",
                "- A2 has no test\n",
                "- R3 is unfalsifiable\n",
            )
        );
    }

    #[test]
    fn a_passing_review_of_the_current_plan_is_fresh() {
        let plan = freezable_plan();
        let rendered = plan_markdown(&plan).unwrap();
        assert!(
            rendered.contains("Cold consumer: present, passed, fresh"),
            "{rendered}"
        );
        assert!(
            rendered.contains("Critic: present, passed, fresh"),
            "{rendered}"
        );
    }

    #[test]
    fn recording_a_review_does_not_make_the_other_review_stale() {
        let mut plan = freezable_plan();
        plan.reviews.critic = None;
        let rendered = plan_markdown(&plan).unwrap();
        assert!(
            rendered.contains("Cold consumer: present, passed, fresh"),
            "{rendered}"
        );
        assert!(rendered.contains("Critic: absent"), "{rendered}");
    }

    #[test]
    fn a_freezable_plan_prints_the_line_the_user_must_type() {
        let plan = freezable_plan();
        let rendered = plan_markdown(&plan).unwrap();
        let challenge = plan.challenge().unwrap();
        assert_eq!(
            section(&rendered, "## Confirm"),
            format!(
                "## Confirm\n\nType this line exactly:\n\n```\nCONFIRM PLAN {challenge}\n```\n"
            )
        );
        assert_eq!(challenge.len(), 8);
    }

    #[test]
    fn an_unfreezable_plan_lists_its_blockers_and_never_the_confirm_line() {
        let mut plan = freezable_plan();
        plan.decisions[0].answer = None;
        let rendered = plan_markdown(&plan).unwrap();
        assert!(
            rendered.contains("- unanswered_decision: C1 is unanswered"),
            "{rendered}"
        );
        assert!(
            !rendered.contains(&format!("CONFIRM PLAN {}", plan.challenge().unwrap())),
            "{rendered}"
        );
        assert!(!rendered.contains("```"), "{rendered}");
    }

    #[test]
    fn every_freeze_blocker_code_is_reachable_and_named_in_the_document() {
        /// A mutation that removes exactly one freeze precondition.
        type Break = Box<dyn Fn(&mut Plan)>;

        let cases: Vec<(&str, Break)> = vec![
            (
                "empty_field",
                Box::new(|p: &mut Plan| p.goal.statement = "   ".into()),
            ),
            (
                "unanswered_decision",
                Box::new(|p: &mut Plan| p.decisions[0].answer = None),
            ),
            (
                "open_item",
                Box::new(|p: &mut Plan| {
                    p.open_items.push(OpenItem {
                        id: "O1".into(),
                        decision_id: "C1".into(),
                        detail: "unresolved".into(),
                    })
                }),
            ),
            ("no_units", Box::new(|p: &mut Plan| p.units.clear())),
            (
                "unit_cycle",
                Box::new(|p: &mut Plan| {
                    p.units = vec![unit("U1", &["U2"]), unit("U2", &["U1"])];
                }),
            ),
            ("no_tests", Box::new(|p: &mut Plan| p.tests.clear())),
            (
                "empty_full_suite",
                Box::new(|p: &mut Plan| p.full_suite = " ".into()),
            ),
            (
                "missing_review",
                Box::new(|p: &mut Plan| p.reviews.critic = None),
            ),
            (
                "failed_review",
                Box::new(|p: &mut Plan| {
                    if let Some(review) = p.reviews.critic.as_mut() {
                        review.passed = false;
                    }
                }),
            ),
            (
                "stale_review",
                Box::new(|p: &mut Plan| {
                    if let Some(review) = p.reviews.critic.as_mut() {
                        review.plan_digest = Digest::zero();
                    }
                }),
            ),
        ];
        for (code, break_it) in cases {
            let mut plan = freezable_plan();
            break_it(&mut plan);
            let rendered = plan_markdown(&plan).unwrap();
            assert!(
                rendered.contains(&format!("- {code}: ")),
                "{code} was not reported in:\n{}",
                section(&rendered, "## Confirm")
            );
        }
    }

    #[test]
    fn blockers_are_reported_in_a_fixed_order() {
        let mut plan = freezable_plan();
        plan.decisions.push(decision("C9"));
        plan.decisions[0].answer = None;
        plan.tests.clear();
        pass_reviews(&mut plan);
        plan.reviews.critic = None;

        let rendered = plan_markdown(&plan).unwrap();
        let blockers: Vec<&str> = section(&rendered, "## Confirm").lines().skip(3).collect();

        // Derived from the gate rather than restated, because that agreement is the property worth
        // holding: a renderer with its own opinion about freezability is how a user ends up
        // confirming a plan the gate rejects.
        let expected: Vec<String> = crate::validate::freeze_blockers(&plan)
            .unwrap()
            .iter()
            .map(|v| format!("- {}: {}", v.code, v.detail))
            .collect();
        assert_eq!(blockers, expected);

        // And the agreement is not vacuous: the substance really is reported.
        for expected_prefix in [
            "- unanswered_decision: C1",
            "- missing_review: ",
            "- untested_unit: ",
        ] {
            assert!(
                blockers.iter().any(|b| b.starts_with(expected_prefix)),
                "{expected_prefix:?} is missing from {blockers:#?}"
            );
        }

        assert_eq!(
            plan_markdown(&plan).unwrap(),
            rendered,
            "the blocker order is not stable across renders"
        );
    }

    #[test]
    fn an_empty_plan_renders_and_omits_every_section_it_has_no_content_for() {
        let plan = Plan::new("g", "main", "goal");
        let rendered = plan_markdown(&plan).unwrap();
        for absent in [
            "## Facts",
            "## Decisions",
            "## Requirements",
            "## Acceptance",
            "## Units",
            "## Tests",
            "## Open items",
        ] {
            assert!(
                !rendered.contains(absent),
                "{absent} should be absent:\n{rendered}"
            );
        }
        for present in ["## Goal", "## Surfaces", "## Reviews", "## Confirm"] {
            assert!(
                rendered.contains(present),
                "{present} should be present:\n{rendered}"
            );
        }
        assert!(rendered.contains("- no_units: "), "{rendered}");
    }

    #[test]
    fn a_json_round_trip_of_the_plan_renders_the_same_bytes() {
        let plan = freezable_plan();
        let decoded: Plan = serde_json::from_str(&serde_json::to_string(&plan).unwrap()).unwrap();
        assert_eq!(
            plan_markdown(&decoded).unwrap(),
            plan_markdown(&plan).unwrap()
        );
        assert_eq!(
            frontier_markdown(&decoded, &["C1".into()]).unwrap(),
            frontier_markdown(&plan, &["C1".into()]).unwrap()
        );
    }

    #[test]
    fn the_whole_document_of_a_small_plan_is_exactly_this() {
        let mut plan = Plan::new("g1", "main", "add dry-run");
        plan.goal.success = vec!["the diff is printed".into()];
        let mut only = decision("C1");
        only.alternatives.truncate(1);
        only.recommendation = Recommendation::NoRecommendation {
            rationale: vec!["only one option survives".into()],
        };
        plan.decisions.push(only);
        let rendered = plan_markdown(&plan).unwrap();
        let digest = plan.digest().unwrap();
        let mut expected = String::new();
        expected.push_str("# add dry-run\n\n");
        expected.push_str("- Goal id: g1\n- Revision: 1\n- Base branch: main\n");
        expected.push_str(&format!("- Plan digest: {digest}\n"));
        expected.push_str(&format!("- Challenge: {}\n\n", digest.challenge()));
        expected.push_str("## Goal\n\nadd dry-run\n\nSuccess:\n- the diff is printed\n\n");
        expected.push_str("## Surfaces\n\n");
        expected.push_str("| Surface | Title | Status | Reason |\n");
        expected.push_str("| --- | --- | --- | --- |\n");
        for surface in SURFACES {
            expected.push_str(&format!(
                "| {} | {} | applicable |  |\n",
                surface.id(),
                surface.title()
            ));
        }
        expected.push_str("\n## Decisions\n\n### C1 · S2 · decision\n\n");
        expected.push_str("Call the admission webhook during dry-run?\n\n- ALT1 — call it\n\n");
        expected.push_str("Recommendation: none\n\nRationale:\n- only one option survives\n\n");
        expected.push_str("Answer: (unanswered)\n\n");
        expected.push_str("## Reviews\n\nCold consumer: absent\n\nCritic: absent\n\n");
        expected.push_str("## Confirm\n\nThe plan cannot be frozen yet:\n");
        // The blockers themselves are the gate's words, not the renderer's, so they are derived
        // rather than restated here — `blockers_are_reported_in_a_fixed_order` is what holds the
        // renderer and the gate to the same answer. Everything above this line is the document
        // shape, and that is what this test exists to pin byte for byte.
        for violation in crate::validate::freeze_blockers(&plan).unwrap() {
            expected.push_str(&format!("- {}: {}\n", violation.code, violation.detail));
        }
        assert_eq!(rendered, expected);

        // A plan this incomplete must not be one line away from being confirmed.
        assert!(!rendered.contains("CONFIRM PLAN"));
    }

    #[test]
    fn the_frontier_renders_the_documented_answer_prompt_shape() {
        let mut plan = freezable_plan();
        plan.decisions[0].question = "dry-run 중 admission webhook을 호출할 것인가?".into();
        plan.decisions[0].alternatives = vec![
            Alternative {
                id: "ALT1".into(),
                value: "호출한다".into(),
            },
            Alternative {
                id: "ALT2".into(),
                value: "호출하지 않는다".into(),
            },
        ];
        let rendered = frontier_markdown(&plan, &["C1".into()]).unwrap();
        assert_eq!(
            rendered,
            concat!(
                "C1. dry-run 중 admission webhook을 호출할 것인가?\n",
                "\n",
                "ALT1. 호출한다\n",
                "ALT2. 호출하지 않는다\n",
                "\n",
                "Recommendation: ALT1\n",
                "Rationale: keeps validation parity with apply\n",
                "Evidence: F7\n",
                "Trade-offs: a webhook failure becomes a dry-run failure\n",
                "Impact: api, tests\n",
                "Confidence: high\n",
                "\n",
                "Answer forms:\n",
                "- C<n>=REC — take the recommendation as displayed\n",
                "- C<n>=ALT<m> — take that alternative\n",
                "- C<n>=OTHER: <value> — answer in your own words\n",
                "- C<n>=UNKNOWN — record it as an open item\n",
                "- S<n>=NA: <reason> — close a surface as not applicable\n",
            )
        );
    }

    #[test]
    fn the_frontier_keeps_the_callers_order_and_skips_ids_the_plan_does_not_have() {
        let mut plan = freezable_plan();
        plan.decisions.push(decision("C3"));
        let rendered =
            frontier_markdown(&plan, &["C3".into(), "C404".into(), "C1".into()]).unwrap();
        let questions: Vec<&str> = rendered
            .lines()
            .filter(|line| line.starts_with("C") && line.contains(". "))
            .collect();
        assert_eq!(
            questions,
            vec![
                "C3. Call the admission webhook during dry-run?",
                "C1. Call the admission webhook during dry-run?",
            ]
        );
        assert!(!rendered.contains("C404"), "{rendered}");
    }

    #[test]
    fn the_frontier_lists_the_answer_forms_once_even_when_nothing_is_ready() {
        let plan = freezable_plan();
        let empty = frontier_markdown(&plan, &[]).unwrap();
        assert_eq!(empty.matches("Answer forms:").count(), 1, "{empty}");
        assert!(empty.starts_with("Answer forms:\n"), "{empty}");

        let both = frontier_markdown(&plan, &["C1".into(), "C1".into()]).unwrap();
        assert_eq!(both.matches("Answer forms:").count(), 1, "{both}");
        assert_eq!(both.matches("C1. Call").count(), 2, "{both}");
    }

    #[test]
    fn the_frontier_renders_the_two_non_recommending_modes_compactly() {
        let mut plan = freezable_plan();
        plan.decisions[0].recommendation = Recommendation::NoRecommendation {
            rationale: vec!["no basis".into(), "genuinely a taste call".into()],
        };
        let none = frontier_markdown(&plan, &["C1".into()]).unwrap();
        assert!(
            none.contains("Recommendation: none\nRationale: no basis; genuinely a taste call\n"),
            "{none}"
        );
        assert!(!none.contains("Confidence:"), "{none}");

        plan.decisions[0].recommendation = Recommendation::ProbeRequired {
            probe_unit: "U9".into(),
            rationale: vec![],
        };
        let probe = frontier_markdown(&plan, &["C1".into()]).unwrap();
        assert!(
            probe.contains("Recommendation: probe U9 first\n"),
            "{probe}"
        );
        assert!(!probe.contains("Rationale:"), "{probe}");
    }

    #[test]
    fn the_frontier_flattens_a_question_that_spans_lines() {
        let mut plan = freezable_plan();
        plan.decisions[0].question = "first\nsecond".into();
        plan.decisions[0].alternatives[0].value = "a\nb".into();
        let rendered = frontier_markdown(&plan, &["C1".into()]).unwrap();
        assert!(rendered.contains("C1. first second\n"), "{rendered}");
        assert!(rendered.contains("ALT1. a b\n"), "{rendered}");
    }

    #[test]
    fn the_frontier_of_a_decision_with_no_alternatives_omits_the_alternative_block() {
        let mut plan = freezable_plan();
        plan.decisions[0].alternatives.clear();
        let rendered = frontier_markdown(&plan, &["C1".into()]).unwrap();
        assert!(
            rendered.starts_with(
                "C1. Call the admission webhook during dry-run?\n\nRecommendation: ALT1\n"
            ),
            "{rendered}"
        );
    }

    #[test]
    fn identifier_ordering_is_total_for_adversarial_ids() {
        let mut ids = vec![
            "C10".to_string(),
            "C2".to_string(),
            "C02".to_string(),
            "C".to_string(),
            String::new(),
            "C99999999999999999999999".to_string(),
            "한글1".to_string(),
        ];
        let sorted: Vec<String> = sorted_ids(&ids).into_iter().map(str::to_string).collect();
        assert_eq!(
            sorted,
            vec![
                "한글1",
                "C02",
                "C2",
                "C10",
                "",
                "C",
                "C99999999999999999999999"
            ]
        );
        ids.reverse();
        assert_eq!(
            sorted_ids(&ids),
            sorted,
            "the order depends on the input order"
        );
    }

    #[test]
    fn a_cell_flattens_controls_and_escapes_separators() {
        assert_eq!(cell("plain"), "plain");
        assert_eq!(cell("a|b"), "a\\|b");
        assert_eq!(cell("a\\b"), "a\\\\b");
        assert_eq!(cell("a\nb\rc\td\u{0}e"), "a b c d e");
        assert_eq!(cell("한글 é"), "한글 é");
        assert_eq!(cell(""), "");
    }

    #[test]
    fn inline_flattens_controls_and_leaves_separators_alone() {
        assert_eq!(inline("a|b"), "a|b");
        assert_eq!(inline("a\u{9}b\u{85}c"), "a b c");
        assert_eq!(inline("é한"), "é한");
    }
}
