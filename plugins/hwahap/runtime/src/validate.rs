//! The freeze gate.
//!
//! `CONFIRM PLAN` turns the plan into a contract the coding engine executes without asking the user
//! anything more, so every rule here is a variant of one question: could an engine run this plan to
//! the end without needing a decision nobody made?
//!
//! Two rules shape the whole module. Every failure is reported rather than only the first, because a
//! user who fixes one blocker and is then shown the next has been made to do the work in rounds. And
//! nothing is assumed: an unrecognised surface, a reference to an id the plan does not contain, or a
//! review bound to a plan that has since changed is a violation, never a silent default.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::{Error, Result};
use crate::plan::{
    AnswerFreshness, DecisionKind, Plan, Recommendation, Selection, Surface, SCHEMA, SURFACES,
};

/// One reason the plan cannot be frozen (or is not even well-formed).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Violation {
    /// A stable machine code, e.g. "unanswered_decision". Codes are snake_case and unique per rule.
    pub code: &'static str,
    /// A sentence naming the exact offending ids.
    pub detail: String,
}

impl Violation {
    fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Violation {
            code,
            detail: detail.into(),
        }
    }
}

/// Structural problems: malformed or duplicated ids, references to things that do not exist.
/// These are bugs in how the plan was built, and they are checked before the freeze gates.
pub fn structural_errors(plan: &Plan) -> Result<Vec<Violation>> {
    let mut out = Vec::new();
    check_schema(plan, &mut out);
    check_surfaces(plan, &mut out);
    check_ids(plan, &mut out);
    check_empty_fields(plan, &mut out);
    check_references(plan, &mut out);
    check_cycles(plan, &mut out);
    Ok(sorted_unique(out))
}

/// Everything that must be true before `CONFIRM PLAN` may be issued.
/// Includes `structural_errors`. Empty means the plan is freezable.
pub fn freeze_blockers(plan: &Plan) -> Result<Vec<Violation>> {
    let mut out = structural_errors(plan)?;
    check_substance(plan, &mut out);
    check_surface_coverage(plan, &mut out)?;
    check_answers(plan, &mut out)?;
    check_traceability(plan, &mut out)?;
    check_verification(plan, &mut out);
    check_reviews(plan, &mut out)?;
    Ok(sorted_unique(out))
}

/// Explicit BUILD keeps executable scope and verification checks without inventing planning evidence.
pub fn build_blockers(plan: &Plan) -> Result<Vec<Violation>> {
    let mut out = structural_errors(plan)?;
    check_substance(plan, &mut out);
    check_traceability(plan, &mut out)?;
    check_verification(plan, &mut out);
    if plan.acceptance.iter().any(|a| a.requirement_ids.is_empty())
        || plan
            .units
            .iter()
            .any(|u| !u.probe && u.acceptance_ids.is_empty())
        || plan.tests.iter().any(|t| t.acceptance_ids.is_empty())
    {
        out.push(Violation::new(
            "empty_build_trace",
            "BUILD acceptance, units and tests need explicit traceability edges",
        ));
    }
    if plan
        .execution_authorization
        .as_ref()
        .is_none_or(|text| is_blank(text))
    {
        out.push(Violation::new(
            "missing_build_authorization",
            "BUILD requires the user's explicit instruction",
        ));
    }
    Ok(sorted_unique(out))
}

/// Validate the imported approval and fresh independent translation reviews before execution.
pub fn approved_plan_blockers(plan: &Plan) -> Result<Vec<Violation>> {
    let mut out = build_blockers(plan)?;
    let valid = plan.approved_plan.as_ref().is_some_and(|a| {
        a.validate().is_ok()
            && plan.execution_authorization.as_ref() == Some(&a.implementation_request)
            && plan.source_head.as_ref() == Some(&a.source_head)
            && plan
                .execution_branch
                .as_ref()
                .is_some_and(|b| b.starts_with("codex/"))
            && !plan.plan_only
            && !plan.interactive
            && !plan.structure_stale
            && plan.open_items.is_empty()
    });
    if !valid {
        out.push(Violation::new(
            "invalid_plan_approval",
            "approved source, execution authority or translated contract is invalid",
        ));
    }
    check_reviews(plan, &mut out)?;
    Ok(sorted_unique(out))
}

/// Units in a deterministic topological order.
///
/// Ties are broken by the numeric suffix, so `U2` precedes `U10` and the result does not depend on
/// the order the units happen to sit in.
///
/// Errs when the graph does not describe one unambiguous order: a cycle, a dependency on a unit that
/// is not in the plan, or a duplicated unit id. [`structural_errors`] reports all three as well;
/// refusing here too keeps a caller from running half a plan in an order it invented.
pub fn unit_order(plan: &Plan) -> Result<Vec<String>> {
    let mut pending: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for unit in &plan.units {
        let deps = unit.depends_on.iter().map(String::as_str).collect();
        if pending.insert(unit.id.as_str(), deps).is_some() {
            return Err(Error::Rejected(format!(
                "unit id {} appears more than once, so there is no single order to run",
                unit.id
            )));
        }
    }
    for (unit, deps) in &pending {
        for dep in deps {
            if !pending.contains_key(dep) {
                return Err(Error::Rejected(format!(
                    "{unit} depends on {dep}, which is not a unit in this plan"
                )));
            }
        }
    }

    let mut order = Vec::with_capacity(pending.len());
    while !pending.is_empty() {
        let ready = pending
            .iter()
            .filter(|(_, deps)| deps.is_empty())
            .map(|(id, _)| *id)
            .min_by_key(|id| order_key(id));
        let Some(next) = ready else {
            // Every remaining unit waits on another remaining unit, so a cycle exists and
            // `cycle_members` cannot come back empty.
            return Err(Error::Rejected(format!(
                "unit dependencies cycle through {}",
                cycle_members(&pending).join(", ")
            )));
        };
        pending.remove(next);
        for deps in pending.values_mut() {
            deps.remove(next);
        }
        order.push(next.to_string());
    }
    Ok(order)
}

/// Sorts by `(code, detail)` and drops exact repeats, so the report is a list of distinct facts in a
/// stable order no matter which rule found what first.
fn sorted_unique(mut violations: Vec<Violation>) -> Vec<Violation> {
    violations.sort();
    violations.dedup();
    violations
}

/// A field nobody filled in.
///
/// Whitespace and control characters are what `char` can classify without a Unicode table, so a
/// zero-width format character counts as content here; inventing a partial list of invisible
/// codepoints would be a rule nobody could predict.
fn is_blank(text: &str) -> bool {
    text.chars().all(|c| c.is_whitespace() || c.is_control())
}

/// The `<n>` of an id spelled exactly `<prefix><n>`.
///
/// Leading zeros, a sign, surrounding whitespace and non-ASCII digits are all rejected: two
/// spellings of the same number would read as one id in a message and as two ids in a set. A number
/// too large for `u64` is rejected too, because the ordering key cannot hold it.
fn numeric_suffix(id: &str, prefix: &str) -> Option<u64> {
    let digits = id.strip_prefix(prefix)?;
    if digits.is_empty() || digits.starts_with('0') || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u64>().ok()
}

fn id_is_well_formed(id: &str, prefix: &str) -> bool {
    numeric_suffix(id, prefix).is_some()
}

/// Orders `U<n>` numerically, with a malformed id last so it cannot displace a well-formed one.
fn order_key(id: &str) -> (u64, &str) {
    (numeric_suffix(id, "U").unwrap_or(u64::MAX), id)
}

/// Why a path may not be a unit's scope.
fn path_problem(path: &str) -> Option<&'static str> {
    if is_blank(path) {
        return Some("is empty");
    }
    // Backslashes count as separators as well: a unit's paths are repository-relative, and a
    // Windows-style `..\` would otherwise walk out of the repository unnoticed.
    if path.starts_with('/') || path.starts_with('\\') {
        return Some("is absolute");
    }
    if path.split(['/', '\\']).any(|part| part == "..") {
        return Some("escapes the repository with a `..` component");
    }
    // `plan.md` renders a unit's scope as one cell of backtick-quoted, comma-separated paths, and
    // that cell is the only place the document says what the unit may rewrite. A path carrying
    // either delimiter would let one path read as two, so the document would understate the scope
    // the user is about to confirm.
    if path.contains(',') || path.contains('`') {
        return Some("carries a comma or a backtick, which separate the paths in plan.md");
    }
    None
}

fn check_schema(plan: &Plan, out: &mut Vec<Violation>) {
    if plan.schema != SCHEMA {
        out.push(Violation::new(
            "schema",
            format!(
                "the plan declares schema {:?}, but {SCHEMA} is required",
                plan.schema
            ),
        ));
    }
}

fn check_surfaces(plan: &Plan, out: &mut Vec<Violation>) {
    for surface in SURFACES {
        if !plan.surfaces.contains_key(surface.id()) {
            out.push(Violation::new(
                "missing_surface",
                format!("surface {surface} is absent from the plan"),
            ));
        }
    }
    for key in plan.surfaces.keys() {
        if Surface::parse(key).is_none() {
            out.push(Violation::new(
                "unknown_surface",
                format!("{key:?} is not one of S1..S12"),
            ));
        }
    }
}

fn check_ids(plan: &Plan, out: &mut Vec<Violation>) {
    let facts: Vec<&str> = plan.facts.iter().map(|f| f.id.as_str()).collect();
    let decisions: Vec<&str> = plan.decisions.iter().map(|d| d.id.as_str()).collect();
    let requirements: Vec<&str> = plan.requirements.iter().map(|r| r.id.as_str()).collect();
    let acceptance: Vec<&str> = plan.acceptance.iter().map(|a| a.id.as_str()).collect();
    let units: Vec<&str> = plan.units.iter().map(|u| u.id.as_str()).collect();
    let tests: Vec<&str> = plan.tests.iter().map(|t| t.id.as_str()).collect();
    check_ids_of(&facts, "F", "fact", out);
    check_ids_of(&decisions, "C", "decision", out);
    check_ids_of(&requirements, "R", "requirement", out);
    check_ids_of(&acceptance, "A", "acceptance", out);
    check_ids_of(&units, "U", "unit", out);
    check_ids_of(&tests, "T", "test", out);

    // An open item's id carries no prefix contract, but two of them sharing an id still makes the
    // report ambiguous about which one is still open.
    let open_items: Vec<&str> = plan.open_items.iter().map(|o| o.id.as_str()).collect();
    for (id, count) in repeats(&open_items) {
        out.push(Violation::new(
            "duplicate_id",
            format!("open item id {id} appears {count} times"),
        ));
    }

    for decision in &plan.decisions {
        let alternatives: Vec<&str> = decision
            .alternatives
            .iter()
            .map(|a| a.id.as_str())
            .collect();
        for id in &alternatives {
            if !id_is_well_formed(id, "ALT") {
                out.push(Violation::new(
                    "malformed_id",
                    format!(
                        "{} alternative id {id:?} is not ALT<n> with n >= 1 and no leading zeros",
                        decision.id
                    ),
                ));
            }
        }
        for (id, count) in repeats(&alternatives) {
            out.push(Violation::new(
                "duplicate_alternative",
                format!("{} has {count} alternatives with id {id}", decision.id),
            ));
        }
        if decision.alternatives.len() < 2 {
            out.push(Violation::new(
                "duplicate_alternative",
                format!(
                    "{} needs at least two alternatives but has {}",
                    decision.id,
                    decision.alternatives.len()
                ),
            ));
        }
    }
}

fn check_ids_of(ids: &[&str], prefix: &str, noun: &str, out: &mut Vec<Violation>) {
    for id in ids {
        if !id_is_well_formed(id, prefix) {
            out.push(Violation::new(
                "malformed_id",
                format!("{noun} id {id:?} is not {prefix}<n> with n >= 1 and no leading zeros"),
            ));
        }
    }
    for (id, count) in repeats(ids) {
        out.push(Violation::new(
            "duplicate_id",
            format!("{noun} id {id} appears {count} times"),
        ));
    }
}

/// The ids that appear more than once, with their counts, in id order.
fn repeats<'a>(ids: &[&'a str]) -> BTreeMap<&'a str, usize> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for id in ids {
        *counts.entry(*id).or_default() += 1;
    }
    counts.retain(|_, count| *count > 1);
    counts
}

fn check_empty_fields(plan: &Plan, out: &mut Vec<Violation>) {
    if is_blank(&plan.goal.statement) {
        out.push(Violation::new("empty_field", "the goal statement is empty"));
    }
    for fact in &plan.facts {
        if fact.sources.is_empty() || fact.sources.iter().any(|source| is_blank(source)) {
            out.push(Violation::new(
                "empty_field",
                format!("{} needs nonempty, nonblank source locations", fact.id),
            ));
        }
    }
    for decision in &plan.decisions {
        for field in crate::proposal::missing_recommendation_fields(&decision.recommendation) {
            out.push(Violation::new(
                "empty_field",
                format!(
                    "{} needs nonempty, nonblank recommendation {field}",
                    decision.id
                ),
            ));
        }
        if is_blank(&decision.question) {
            out.push(Violation::new(
                "empty_field",
                format!("{} has an empty question", decision.id),
            ));
        }
        for alternative in &decision.alternatives {
            if is_blank(&alternative.value) {
                out.push(Violation::new(
                    "empty_field",
                    format!(
                        "{} alternative {} has an empty value",
                        decision.id, alternative.id
                    ),
                ));
            }
        }
    }
    for requirement in &plan.requirements {
        if is_blank(&requirement.statement) {
            out.push(Violation::new(
                "empty_field",
                format!("{} has an empty statement", requirement.id),
            ));
        }
    }
    for acceptance in &plan.acceptance {
        if is_blank(&acceptance.observable) {
            out.push(Violation::new(
                "empty_field",
                format!("{} has an empty observable", acceptance.id),
            ));
        }
    }
    for unit in &plan.units {
        if is_blank(&unit.title) {
            out.push(Violation::new(
                "empty_field",
                format!("{} has an empty title", unit.id),
            ));
        }
        if unit.paths.is_empty() {
            out.push(Violation::new(
                "empty_field",
                format!("{} lists no paths", unit.id),
            ));
        }
        for path in &unit.paths {
            if let Some(problem) = path_problem(path) {
                out.push(Violation::new(
                    "empty_field",
                    format!("{} lists the path {path:?}, which {problem}", unit.id),
                ));
            }
        }
    }
    for test in &plan.tests {
        if is_blank(&test.command) {
            out.push(Violation::new(
                "empty_field",
                format!("{} has an empty command", test.id),
            ));
        }
    }
}

fn check_references(plan: &Plan, out: &mut Vec<Violation>) {
    // Built from the ids the plan actually carries, malformed or duplicated ones included: an id
    // that is present is not dangling, and its own rule already reports its own problem.
    let facts = id_set(plan.facts.iter().map(|f| f.id.as_str()));
    let decisions = id_set(plan.decisions.iter().map(|d| d.id.as_str()));
    let requirements = id_set(plan.requirements.iter().map(|r| r.id.as_str()));
    let acceptance = id_set(plan.acceptance.iter().map(|a| a.id.as_str()));
    let units = id_set(plan.units.iter().map(|u| u.id.as_str()));

    for decision in &plan.decisions {
        for dep in &decision.depends_on {
            require(
                &decisions,
                dep,
                format!("{} depends on {dep}", decision.id),
                out,
            );
        }
        match &decision.recommendation {
            Recommendation::Recommended {
                choice, evidence, ..
            } => {
                if !decision.alternatives.iter().any(|a| &a.id == choice) {
                    out.push(Violation::new(
                        "dangling_reference",
                        format!(
                            "{} recommends {choice}, which is not one of its own alternatives",
                            decision.id
                        ),
                    ));
                }
                for fact in evidence {
                    require(
                        &facts,
                        fact,
                        format!("{} cites evidence {fact}", decision.id),
                        out,
                    );
                }
            }
            Recommendation::ProbeRequired { probe_unit, .. } => {
                require(
                    &units,
                    probe_unit,
                    format!("{} names probe unit {probe_unit}", decision.id),
                    out,
                );
            }
            Recommendation::NoRecommendation { .. } => {}
        }
    }
    for requirement in &plan.requirements {
        for id in &requirement.decision_ids {
            require(
                &decisions,
                id,
                format!("{} cites decision {id}", requirement.id),
                out,
            );
        }
    }
    for item in &plan.acceptance {
        for id in &item.requirement_ids {
            require(
                &requirements,
                id,
                format!("{} cites requirement {id}", item.id),
                out,
            );
        }
    }
    for unit in &plan.units {
        for id in &unit.acceptance_ids {
            require(
                &acceptance,
                id,
                format!("{} cites acceptance {id}", unit.id),
                out,
            );
        }
        for id in &unit.depends_on {
            require(&units, id, format!("{} depends on {id}", unit.id), out);
        }
    }
    for test in &plan.tests {
        for id in &test.acceptance_ids {
            require(
                &acceptance,
                id,
                format!("{} cites acceptance {id}", test.id),
                out,
            );
        }
        require(
            &units,
            &test.unit_id,
            format!("{} belongs to unit {}", test.id, test.unit_id),
            out,
        );
    }
    for item in &plan.open_items {
        require(
            &decisions,
            &item.decision_id,
            format!("open item {} cites decision {}", item.id, item.decision_id),
            out,
        );
    }
}

fn id_set<'a>(ids: impl Iterator<Item = &'a str>) -> BTreeSet<&'a str> {
    ids.collect()
}

fn require(known: &BTreeSet<&str>, id: &str, sentence: String, out: &mut Vec<Violation>) {
    if !known.contains(id) {
        out.push(Violation::new(
            "dangling_reference",
            format!("{sentence}, which does not exist in this plan"),
        ));
    }
}

fn check_cycles(plan: &Plan, out: &mut Vec<Violation>) {
    let decisions = id_set(plan.decisions.iter().map(|d| d.id.as_str()));
    let mut graph: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for decision in &plan.decisions {
        // A dependency on an id the plan does not have is a dangling reference, not a cycle; leaving
        // it in the graph would make every unschedulable dependent look like a cycle member.
        let deps = decision
            .depends_on
            .iter()
            .map(String::as_str)
            .filter(|id| decisions.contains(id));
        graph.entry(decision.id.as_str()).or_default().extend(deps);
    }
    let members = cycle_members(&graph);
    if !members.is_empty() {
        out.push(Violation::new(
            "decision_cycle",
            format!("decision dependencies cycle through {}", members.join(", ")),
        ));
    }

    let units = id_set(plan.units.iter().map(|u| u.id.as_str()));
    let mut graph: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for unit in &plan.units {
        let deps = unit
            .depends_on
            .iter()
            .map(String::as_str)
            .filter(|id| units.contains(id));
        graph.entry(unit.id.as_str()).or_default().extend(deps);
    }
    let members = cycle_members(&graph);
    if !members.is_empty() {
        out.push(Violation::new(
            "unit_cycle",
            format!("unit dependencies cycle through {}", members.join(", ")),
        ));
    }
}

/// The nodes that sit on a dependency cycle, sorted.
///
/// `graph` maps a node to its dependencies, which must all be nodes of the graph.
fn cycle_members(graph: &BTreeMap<&str, BTreeSet<&str>>) -> Vec<String> {
    let mut blocked = graph.clone();
    // Drop everything that can be ordered, leaving the nodes a cycle blocks.
    loop {
        let ready: Vec<&str> = blocked
            .iter()
            .filter(|(_, deps)| deps.is_empty())
            .map(|(id, _)| *id)
            .collect();
        if ready.is_empty() {
            break;
        }
        for id in &ready {
            blocked.remove(id);
            for deps in blocked.values_mut() {
                deps.remove(id);
            }
        }
    }
    // Then drop the nodes that merely wait on a cycle, so the message names the cycle itself rather
    // than everything downstream of it.
    loop {
        let leaves: Vec<&str> = blocked
            .keys()
            .copied()
            .filter(|id| !blocked.values().any(|deps| deps.contains(id)))
            .collect();
        if leaves.is_empty() {
            break;
        }
        for id in &leaves {
            blocked.remove(id);
        }
    }
    blocked.keys().map(|id| (*id).to_string()).collect()
}

/// The plan has to build something.
///
/// Every other completeness rule walks a collection — `orphan_requirement` over requirements,
/// `untested_unit` over units, `check_surface_coverage` over applicable surfaces — so all of them
/// hold vacuously over a plan that states nothing. Closing all twelve surfaces with `S<n>=NA` and
/// proposing an empty structure is enough to reach that state, and without these four rules the gate
/// would hand such a plan a `CONFIRM PLAN` line for a run with no unit to implement.
fn check_substance(plan: &Plan, out: &mut Vec<Violation>) {
    if plan.requirements.is_empty() {
        out.push(Violation::new(
            "no_requirements",
            "the plan states no requirement to implement",
        ));
    }
    if plan.acceptance.is_empty() {
        out.push(Violation::new(
            "no_acceptance",
            "the plan states no acceptance criterion to meet",
        ));
    }
    if plan.units.is_empty() {
        out.push(Violation::new(
            "no_units",
            "the plan has no unit to implement",
        ));
    }
    if plan.tests.is_empty() {
        out.push(Violation::new(
            "no_tests",
            "the plan has no test to prove any acceptance",
        ));
    }
}

fn check_surface_coverage(plan: &Plan, out: &mut Vec<Violation>) -> Result<()> {
    for surface in plan.applicable_surfaces() {
        let mut answered = false;
        let mut scenario = false;
        for decision in plan.decisions_on(surface) {
            if decision.is_answered()? {
                answered |= decision.kind == DecisionKind::Decision;
                scenario |= decision.kind == DecisionKind::Scenario;
            }
        }
        // Both conditions are reported when both fail: the user needs to know the surface still owes
        // a scenario before answering, not after.
        if !answered {
            out.push(Violation::new(
                "unanswered_surface",
                format!("{surface} has no answered decision of kind decision"),
            ));
        }
        if !scenario {
            out.push(Violation::new(
                "unanswered_surface",
                format!("{surface} has no answered decision of kind scenario"),
            ));
        }
    }
    Ok(())
}

fn check_answers(plan: &Plan, out: &mut Vec<Violation>) -> Result<()> {
    let applicable: BTreeSet<Surface> = plan.applicable_surfaces().into_iter().collect();
    for decision in &plan.decisions {
        if applicable.contains(&decision.surface) {
            let freshness = decision.answer_freshness()?;
            if freshness != AnswerFreshness::Fresh {
                out.push(Violation::new(
                    "unanswered_decision",
                    freshness.explain(&decision.id),
                ));
            }
        }

        // Unlike the gates above, this one ignores whether the surface is applicable: UNKNOWN is the
        // user admitting they do not know, and that admission does not expire with a closed surface.
        let is_unknown = matches!(&decision.answer, Some(a) if a.selection == Selection::Unknown);
        if is_unknown && decision.is_answered()? {
            let has_open_item = plan.open_items.iter().any(|o| o.decision_id == decision.id);
            let has_probe = match &decision.recommendation {
                Recommendation::ProbeRequired { probe_unit, .. } => plan.unit(probe_unit).is_some(),
                _ => false,
            };
            if !has_open_item && !has_probe {
                out.push(Violation::new(
                    "unresolved_unknown",
                    format!(
                        "{} is answered UNKNOWN with no open item and no probe unit to resolve it",
                        decision.id
                    ),
                ));
            }
        }

        if let Recommendation::ProbeRequired { probe_unit, .. } = &decision.recommendation {
            // A probe unit that does not exist is a dangling reference, reported there.
            if let Some(unit) = plan.unit(probe_unit) {
                if !unit.probe {
                    out.push(Violation::new(
                        "probe_not_scheduled",
                        format!(
                            "{} needs probe unit {probe_unit}, which is not marked as a probe",
                            decision.id
                        ),
                    ));
                }
            }
        }
    }

    for item in &plan.open_items {
        out.push(Violation::new(
            "open_item",
            format!(
                "open item {} on {} is unresolved: {}",
                item.id, item.decision_id, item.detail
            ),
        ));
    }
    Ok(())
}

fn check_traceability(plan: &Plan, out: &mut Vec<Violation>) -> Result<()> {
    let applicable: BTreeSet<Surface> = plan.applicable_surfaces().into_iter().collect();
    let cited_decisions = id_set(
        plan.requirements
            .iter()
            .flat_map(|r| r.decision_ids.iter().map(String::as_str)),
    );
    for decision in &plan.decisions {
        if applicable.contains(&decision.surface)
            && decision.is_answered()?
            && !cited_decisions.contains(decision.id.as_str())
        {
            out.push(Violation::new(
                "orphan_decision",
                format!("{} is answered but no requirement cites it", decision.id),
            ));
        }
    }

    // The other direction of the same edge. `orphan_decision` asks whether every answer reached a
    // requirement; this asks whether every requirement came from an answer. Without it a requirement
    // may claim an origin it does not have — a decision on a closed surface that nobody ever
    // answered — and the coding engine would implement it unattended.
    for requirement in &plan.requirements {
        for id in &requirement.decision_ids {
            // A decision the plan does not contain is a dangling reference, reported there.
            if let Some(decision) = plan.decision(id) {
                if !decision.is_answered()? {
                    out.push(Violation::new(
                        "ungrounded_requirement",
                        format!("{} cites {id}, which is not answered", requirement.id),
                    ));
                }
            }
        }
    }

    let cited_requirements = id_set(
        plan.acceptance
            .iter()
            .flat_map(|a| a.requirement_ids.iter().map(String::as_str)),
    );
    for requirement in &plan.requirements {
        if !cited_requirements.contains(requirement.id.as_str()) {
            out.push(Violation::new(
                "orphan_requirement",
                format!("{} is cited by no acceptance", requirement.id),
            ));
        }
    }

    let unit_acceptance = id_set(
        plan.units
            .iter()
            .flat_map(|u| u.acceptance_ids.iter().map(String::as_str)),
    );
    let test_acceptance = id_set(
        plan.tests
            .iter()
            .flat_map(|t| t.acceptance_ids.iter().map(String::as_str)),
    );
    for acceptance in &plan.acceptance {
        if !unit_acceptance.contains(acceptance.id.as_str()) {
            out.push(Violation::new(
                "orphan_acceptance",
                format!("{} is cited by no unit", acceptance.id),
            ));
        }
        if !test_acceptance.contains(acceptance.id.as_str()) {
            out.push(Violation::new(
                "orphan_acceptance",
                format!("{} is cited by no test", acceptance.id),
            ));
        }
    }
    Ok(())
}

fn check_verification(plan: &Plan, out: &mut Vec<Violation>) {
    for unit in &plan.units {
        // A probe is run and thrown away, so demanding a test for it would demand a test for code
        // that will never ship.
        if !unit.probe && plan.tests_for(&unit.id).is_empty() {
            out.push(Violation::new(
                "untested_unit",
                format!("{} has no test", unit.id),
            ));
        }
        if !unit.probe {
            let covered = id_set(
                plan.tests_for(&unit.id)
                    .iter()
                    .flat_map(|test| test.acceptance_ids.iter().map(String::as_str)),
            );
            for id in &unit.acceptance_ids {
                if !covered.contains(id.as_str()) {
                    out.push(Violation::new(
                        "uncovered_unit_acceptance",
                        format!("{} has no own test for acceptance {id}", unit.id),
                    ));
                }
            }
        }
    }
    for test in &plan.tests {
        // A test whose unit does not exist is a dangling reference, reported there; there is no unit
        // scope to compare it against here.
        if let Some(unit) = plan.unit(&test.unit_id) {
            let owned = id_set(unit.acceptance_ids.iter().map(String::as_str));
            for id in &test.acceptance_ids {
                if !owned.contains(id.as_str()) {
                    out.push(Violation::new(
                        "test_outside_unit",
                        format!(
                            "{} cites acceptance {id}, which its unit {} does not cover",
                            test.id, unit.id
                        ),
                    ));
                }
            }
        }
    }
    if is_blank(&plan.full_suite) {
        out.push(Violation::new(
            "empty_full_suite",
            "the full suite command is empty",
        ));
    }
}

fn check_reviews(plan: &Plan, out: &mut Vec<Violation>) -> Result<()> {
    // Reviews bind to `review_digest`, which excludes `reviews` and `frozen`: recording a review
    // changes `digest`, so a review bound to that would be stale the moment it was stored.
    let reviewed = plan.review_digest()?;
    for (name, review) in [
        ("cold_consumer", &plan.reviews.cold_consumer),
        ("critic", &plan.reviews.critic),
    ] {
        match review {
            None => out.push(Violation::new(
                "missing_review",
                format!("the {name} review has not been recorded"),
            )),
            Some(review) => {
                if !review.passed {
                    out.push(Violation::new(
                        "failed_review",
                        format!("the {name} review did not pass"),
                    ));
                }
                if review.plan_digest != reviewed {
                    out.push(Violation::new(
                        "stale_review",
                        format!("the {name} review covers a different plan and must be run again"),
                    ));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::Digest;
    use crate::plan::{
        Acceptance, Alternative, Answer, Confidence, Decision, Fact, OpenItem, PlanReview,
        Requirement, SurfaceStatus, Test, Unit,
    };

    const TS: &str = "2026-09-04T00:00:00Z";

    /// One change applied to a copy of the fixture.
    type Mutation = fn(&mut Plan);

    /// One change that writes the same blank text into a different field each time.
    type BlankMutation = fn(&mut Plan, String);

    fn recommended(choice: &str) -> Recommendation {
        Recommendation::Recommended {
            choice: choice.into(),
            rationale: vec!["it matches how apply already parses flags".into()],
            evidence: vec!["F1".into()],
            tradeoffs: vec!["one more flag to document".into()],
            impact: vec!["cli".into()],
            confidence: Confidence::High,
        }
    }

    fn no_recommendation() -> Recommendation {
        Recommendation::NoRecommendation {
            rationale: vec!["no objective basis".into()],
        }
    }

    fn probe_required(unit: &str) -> Recommendation {
        Recommendation::ProbeRequired {
            probe_unit: unit.into(),
            rationale: vec!["measure before choosing".into()],
        }
    }

    fn decide(
        id: &str,
        surface: Surface,
        kind: DecisionKind,
        recommendation: Recommendation,
    ) -> Decision {
        Decision {
            id: id.into(),
            surface,
            kind,
            question: format!("what should {id} do?"),
            alternatives: vec![
                Alternative {
                    id: "ALT1".into(),
                    value: "the first way".into(),
                },
                Alternative {
                    id: "ALT2".into(),
                    value: "the second way".into(),
                },
            ],
            recommendation,
            depends_on: vec![],
            answer: None,
        }
    }

    fn answer(decision: &Decision, selection: Selection) -> Answer {
        Answer {
            text: format!("{}=...", decision.id),
            recommendation: matches!(selection, Selection::Recommendation)
                .then(|| decision.recommendation_digest().unwrap()),
            selection,
            ts: TS.into(),
            identity: decision.identity_digest().unwrap(),
        }
    }

    fn reanswer(plan: &mut Plan, id: &str, selection: Selection) {
        let decision = plan
            .decision_mut(id)
            .expect("the fixture has this decision");
        let fresh = answer(decision, selection);
        decision.answer = Some(fresh);
    }

    fn record_reviews(plan: &mut Plan) {
        let review = PlanReview {
            plan_digest: plan.review_digest().unwrap(),
            ts: TS.into(),
            passed: true,
            findings: vec![],
        };
        plan.reviews.cold_consumer = Some(review.clone());
        plan.reviews.critic = Some(review);
    }

    /// A plan that passes every rule in this module.
    ///
    /// Only S1 stays applicable; the rest are closed the way a user closes them. Every surface is
    /// gated identically, so a smaller applicable set costs no coverage and keeps the fixture
    /// readable. C3 and C4 live on a closed surface on purpose: they are what proves the gates that
    /// only apply to open surfaces really do skip closed ones.
    fn fixture() -> Plan {
        let mut plan = Plan::new("2026-09-04-dry-run", "main", "add a dry-run flag to apply");
        for surface in SURFACES.into_iter().skip(1) {
            plan.surfaces.insert(
                surface.id().into(),
                SurfaceStatus::NotApplicable {
                    reason: format!("{surface} does not apply to one flag"),
                    answer: Answer {
                        text: format!("{surface}=NA"),
                        selection: Selection::NotApplicable,
                        ts: TS.into(),
                        identity: Digest::zero(),
                        recommendation: None,
                    },
                },
            );
        }
        plan.facts.push(Fact {
            id: "F1".into(),
            question: "does apply already parse flags?".into(),
            answer: "yes, in src/apply/mod.rs".into(),
            sources: vec!["src/apply/mod.rs:12-40".into()],
        });

        let mut c1 = decide(
            "C1",
            Surface::S1,
            DecisionKind::Decision,
            recommended("ALT1"),
        );
        c1.answer = Some(answer(&c1, Selection::Alternative { id: "ALT2".into() }));
        let mut c2 = decide(
            "C2",
            Surface::S1,
            DecisionKind::Scenario,
            recommended("ALT1"),
        );
        c2.answer = Some(answer(&c2, Selection::Recommendation));
        let mut c3 = decide(
            "C3",
            Surface::S2,
            DecisionKind::Decision,
            probe_required("U3"),
        );
        c3.answer = Some(answer(&c3, Selection::Alternative { id: "ALT1".into() }));
        let c4 = decide(
            "C4",
            Surface::S2,
            DecisionKind::Scenario,
            no_recommendation(),
        );
        plan.decisions = vec![c1, c2, c3, c4];

        plan.requirements = vec![
            Requirement {
                id: "R1".into(),
                statement: "apply accepts --dry-run".into(),
                decision_ids: vec!["C1".into()],
            },
            Requirement {
                id: "R2".into(),
                statement: "a rejected value exits non-zero".into(),
                decision_ids: vec!["C2".into()],
            },
        ];
        plan.acceptance = vec![
            Acceptance {
                id: "A1".into(),
                requirement_ids: vec!["R1".into()],
                observable: "apply --dry-run exits 0".into(),
            },
            Acceptance {
                id: "A2".into(),
                requirement_ids: vec!["R2".into()],
                observable: "apply --dry-run=maybe exits 2".into(),
            },
        ];
        plan.units = vec![
            Unit {
                id: "U1".into(),
                title: "parse the flag".into(),
                paths: vec!["src/cli.rs".into()],
                acceptance_ids: vec!["A1".into()],
                depends_on: vec![],
                probe: false,
            },
            Unit {
                id: "U2".into(),
                title: "reject bad values".into(),
                paths: vec!["src/cli.rs".into()],
                acceptance_ids: vec!["A2".into()],
                depends_on: vec!["U1".into()],
                probe: false,
            },
            Unit {
                id: "U3".into(),
                title: "measure webhook latency".into(),
                paths: vec!["src/probe.rs".into()],
                acceptance_ids: vec!["A2".into()],
                depends_on: vec![],
                probe: true,
            },
        ];
        plan.tests = vec![
            Test {
                id: "T1".into(),
                command: "cargo test cli::accepts_dry_run".into(),
                acceptance_ids: vec!["A1".into()],
                unit_id: "U1".into(),
            },
            Test {
                id: "T2".into(),
                command: "cargo test cli::rejects_bad_value".into(),
                acceptance_ids: vec!["A2".into()],
                unit_id: "U2".into(),
            },
        ];
        plan.full_suite = "cargo test".into();
        record_reviews(&mut plan);
        plan
    }

    /// The fixture with one change applied and the reviews re-recorded, so a test that breaks one
    /// rule is not also told its reviews went stale.
    fn mutated(mutate: impl FnOnce(&mut Plan)) -> Plan {
        let mut plan = fixture();
        mutate(&mut plan);
        record_reviews(&mut plan);
        plan
    }

    fn blockers(plan: &Plan) -> Vec<Violation> {
        freeze_blockers(plan).unwrap()
    }

    fn structural(plan: &Plan) -> Vec<Violation> {
        structural_errors(plan).unwrap()
    }

    /// The single violation a one-rule break must produce.
    fn sole(violations: &[Violation]) -> Violation {
        assert_eq!(
            violations.len(),
            1,
            "expected exactly one violation, got {violations:#?}"
        );
        violations[0].clone()
    }

    fn codes(violations: &[Violation]) -> Vec<&str> {
        violations.iter().map(|v| v.code).collect()
    }

    fn details(violations: &[Violation], code: &str) -> Vec<String> {
        violations
            .iter()
            .filter(|v| v.code == code)
            .map(|v| v.detail.clone())
            .collect()
    }

    #[test]
    fn the_fixture_plan_is_freezable() {
        let plan = fixture();
        assert_eq!(structural(&plan), vec![]);
        assert_eq!(blockers(&plan), vec![]);
    }

    #[test]
    fn a_wrong_schema_tag_blocks_the_freeze() {
        let plan = mutated(|p| p.schema = "hwahap/v2".into());
        let violation = sole(&structural(&plan));
        assert_eq!(violation.code, "schema");
        assert_eq!(
            violation.detail,
            "the plan declares schema \"hwahap/v2\", but hwahap/v4 is required"
        );
        assert!(
            blockers(&plan).contains(&violation),
            "the gate must include structural errors"
        );
    }

    #[test]
    fn a_surface_missing_from_the_map_is_reported() {
        let plan = mutated(|p| {
            p.surfaces.remove("S7");
        });
        let violation = sole(&structural(&plan));
        assert_eq!(violation.code, "missing_surface");
        assert_eq!(violation.detail, "surface S7 is absent from the plan");
    }

    #[test]
    fn every_missing_surface_is_named_separately() {
        let plan = mutated(|p| {
            p.surfaces.remove("S7");
            p.surfaces.remove("S11");
        });
        // Details are sorted as text, so S11 sorts before S7. Stability is what matters here.
        assert_eq!(
            details(&structural(&plan), "missing_surface"),
            vec![
                "surface S11 is absent from the plan",
                "surface S7 is absent from the plan"
            ]
        );
    }

    #[test]
    fn a_surface_key_outside_s1_to_s12_is_reported() {
        for bad in [
            "S13", "S0", "s1", " S1", "S1 ", "S01", "S", "", "S1\n", "Ｓ1",
        ] {
            let plan = mutated(|p| {
                p.surfaces.insert(bad.into(), SurfaceStatus::Applicable);
            });
            let violation = sole(&structural(&plan));
            assert_eq!(
                violation.code, "unknown_surface",
                "{bad:?} should be unknown"
            );
            assert_eq!(violation.detail, format!("{bad:?} is not one of S1..S12"));
        }
    }

    #[test]
    fn an_id_must_be_its_prefix_and_a_decimal_without_leading_zeros() {
        let bad = [
            "",
            "F",
            "F0",
            "F01",
            "F1 ",
            " F1",
            "f1",
            "F+1",
            "F1.0",
            "F-1",
            "FF1",
            "F1F",
            "F 1",
            "F\n1",
            "F1\n",
            "F\u{0661}",
            "F\u{ff11}",
            "F18446744073709551616",
            "ALT1",
        ];
        for id in bad {
            let plan = mutated(|p| p.facts.push(fact(id)));
            let violation = sole(&structural(&plan));
            assert_eq!(violation.code, "malformed_id", "{id:?} should be malformed");
            assert_eq!(
                violation.detail,
                format!("fact id {id:?} is not F<n> with n >= 1 and no leading zeros")
            );
        }
        for id in ["F2", "F10", "F18446744073709551615"] {
            let plan = mutated(|p| p.facts.push(fact(id)));
            assert_eq!(structural(&plan), vec![], "{id:?} should be well formed");
        }
    }

    fn fact(id: &str) -> Fact {
        Fact {
            id: id.into(),
            question: "is the flag already parsed?".into(),
            answer: "no".into(),
            sources: vec!["src/cli.rs:1".into()],
        }
    }

    #[test]
    fn a_malformed_id_is_reported_in_every_collection_that_has_one() {
        let plan = mutated(|p| {
            p.facts.push(fact("f1"));
            p.decisions.push(decide(
                "C01",
                Surface::S2,
                DecisionKind::Decision,
                no_recommendation(),
            ));
            p.requirements[1].id = "R2x".into();
            p.acceptance[1].requirement_ids = vec!["R2x".into()];
            p.acceptance[0].id = "A_1".into();
            p.units[0].acceptance_ids = vec!["A_1".into()];
            p.tests[0].acceptance_ids = vec!["A_1".into()];
            p.units.push(Unit {
                id: "U03".into(),
                title: "a second probe".into(),
                paths: vec!["src/probe.rs".into()],
                acceptance_ids: vec!["A2".into()],
                depends_on: vec![],
                probe: true,
            });
            p.tests.push(Test {
                id: "T01".into(),
                command: "cargo test cli::extra".into(),
                acceptance_ids: vec!["A2".into()],
                unit_id: "U2".into(),
            });
            p.decisions[0].alternatives.push(Alternative {
                id: "ALT03".into(),
                value: "a third way".into(),
            });
            reanswer(p, "C1", Selection::Alternative { id: "ALT2".into() });
        });
        assert_eq!(
            details(&blockers(&plan), "malformed_id"),
            vec![
                "C1 alternative id \"ALT03\" is not ALT<n> with n >= 1 and no leading zeros",
                "acceptance id \"A_1\" is not A<n> with n >= 1 and no leading zeros",
                "decision id \"C01\" is not C<n> with n >= 1 and no leading zeros",
                "fact id \"f1\" is not F<n> with n >= 1 and no leading zeros",
                "requirement id \"R2x\" is not R<n> with n >= 1 and no leading zeros",
                "test id \"T01\" is not T<n> with n >= 1 and no leading zeros",
                "unit id \"U03\" is not U<n> with n >= 1 and no leading zeros",
            ]
        );
        assert_eq!(codes(&blockers(&plan)), vec!["malformed_id"; 7]);
    }

    #[test]
    fn a_repeated_id_is_reported_with_how_often_it_appears() {
        let plan = mutated(|p| {
            p.facts.push(fact("F1"));
            p.facts.push(fact("F1"));
        });
        let violation = sole(&structural(&plan));
        assert_eq!(violation.code, "duplicate_id");
        assert_eq!(violation.detail, "fact id F1 appears 3 times");
    }

    #[test]
    fn a_repeated_decision_id_is_reported_even_when_both_copies_are_answered() {
        let plan = mutated(|p| {
            let copy = p.decisions[0].clone();
            p.decisions.push(copy);
        });
        let violation = sole(&blockers(&plan));
        assert_eq!(violation.code, "duplicate_id");
        assert_eq!(violation.detail, "decision id C1 appears 2 times");
    }

    #[test]
    fn a_repeated_open_item_id_is_reported_alongside_the_open_items() {
        let plan = mutated(|p| {
            p.open_items.push(OpenItem {
                id: "O1".into(),
                decision_id: "C1".into(),
                detail: "needs a benchmark".into(),
            });
            p.open_items.push(OpenItem {
                id: "O1".into(),
                decision_id: "C2".into(),
                detail: "needs a second benchmark".into(),
            });
        });
        assert_eq!(
            details(&structural(&plan), "duplicate_id"),
            vec!["open item id O1 appears 2 times"]
        );
    }

    #[test]
    fn two_alternatives_with_the_same_id_are_reported() {
        let plan = mutated(|p| {
            p.decisions[3].alternatives.push(Alternative {
                id: "ALT1".into(),
                value: "a third way spelled as the first".into(),
            });
        });
        let violation = sole(&structural(&plan));
        assert_eq!(violation.code, "duplicate_alternative");
        assert_eq!(violation.detail, "C4 has 2 alternatives with id ALT1");
    }

    #[test]
    fn a_decision_with_fewer_than_two_alternatives_is_not_a_question() {
        for keep in [0, 1] {
            let plan = mutated(|p| p.decisions[3].alternatives.truncate(keep));
            let violation = sole(&structural(&plan));
            assert_eq!(violation.code, "duplicate_alternative");
            assert_eq!(
                violation.detail,
                format!("C4 needs at least two alternatives but has {keep}")
            );
        }
    }

    #[test]
    fn every_kind_of_reference_is_checked_against_what_the_plan_contains() {
        let cases: [(&str, Mutation); 8] = [
            ("C1 depends on C9", |p| {
                p.decisions[0].depends_on.push("C9".into())
            }),
            ("R1 cites decision C9", |p| {
                p.requirements[0].decision_ids.push("C9".into())
            }),
            ("A1 cites requirement R9", |p| {
                p.acceptance[0].requirement_ids.push("R9".into())
            }),
            ("U1 cites acceptance A9", |p| {
                p.units[0].acceptance_ids.push("A9".into())
            }),
            ("U2 depends on U9", |p| {
                p.units[1].depends_on.push("U9".into())
            }),
            ("T3 belongs to unit U9", |p| {
                p.tests.push(Test {
                    id: "T3".into(),
                    command: "cargo test nothing".into(),
                    acceptance_ids: vec![],
                    unit_id: "U9".into(),
                })
            }),
            ("C3 names probe unit U9", |p| {
                p.decisions[2].recommendation = probe_required("U9")
            }),
            ("C1 cites evidence F9", |p| {
                p.decisions[0].recommendation = Recommendation::Recommended {
                    choice: "ALT1".into(),
                    rationale: vec!["still the first way".into()],
                    evidence: vec!["F9".into()],
                    tradeoffs: vec!["one more flag to document".into()],
                    impact: vec!["cli parsing".into()],
                    confidence: Confidence::Low,
                }
            }),
        ];
        for (expected, mutate) in cases {
            let plan = mutated(mutate);
            let violation = sole(&blockers(&plan));
            assert_eq!(violation.code, "dangling_reference", "{expected}");
            assert_eq!(
                violation.detail,
                format!("{expected}, which does not exist in this plan")
            );
        }
    }

    #[test]
    fn a_recommendation_may_only_recommend_one_of_its_own_alternatives() {
        let plan = mutated(|p| p.decisions[0].recommendation = recommended("ALT9"));
        let violation = sole(&structural(&plan));
        assert_eq!(violation.code, "dangling_reference");
        assert_eq!(
            violation.detail,
            "C1 recommends ALT9, which is not one of its own alternatives"
        );
    }

    #[test]
    fn a_test_and_its_unit_are_both_told_about_a_missing_acceptance() {
        let plan = mutated(|p| {
            p.units[0].acceptance_ids.push("A9".into());
            p.tests[0].acceptance_ids.push("A9".into());
        });
        assert_eq!(
            details(&blockers(&plan), "dangling_reference"),
            vec![
                "T1 cites acceptance A9, which does not exist in this plan",
                "U1 cites acceptance A9, which does not exist in this plan",
            ]
        );
        assert_eq!(codes(&blockers(&plan)), vec!["dangling_reference"; 2]);
    }

    #[test]
    fn an_open_item_on_a_decision_that_does_not_exist_is_reported_twice_over() {
        let plan = mutated(|p| {
            p.open_items.push(OpenItem {
                id: "O1".into(),
                decision_id: "C9".into(),
                detail: "who owns the flag?".into(),
            })
        });
        assert_eq!(
            codes(&blockers(&plan)),
            vec!["dangling_reference", "open_item"]
        );
        assert_eq!(
            blockers(&plan)[0].detail,
            "open item O1 cites decision C9, which does not exist in this plan"
        );
    }

    #[test]
    fn decisions_that_depend_on_each_other_are_reported_as_a_cycle() {
        let plan = mutated(|p| {
            p.decisions[0].depends_on = vec!["C2".into()];
            p.decisions[1].depends_on = vec!["C1".into()];
        });
        let violation = sole(&blockers(&plan));
        assert_eq!(violation.code, "decision_cycle");
        assert_eq!(
            violation.detail,
            "decision dependencies cycle through C1, C2"
        );
    }

    #[test]
    fn a_decision_that_depends_on_itself_is_a_cycle_of_one() {
        let plan = mutated(|p| p.decisions[0].depends_on = vec!["C1".into()]);
        let violation = sole(&blockers(&plan));
        assert_eq!(violation.code, "decision_cycle");
        assert_eq!(violation.detail, "decision dependencies cycle through C1");
    }

    #[test]
    fn units_that_depend_on_each_other_are_reported_as_a_cycle() {
        let plan = mutated(|p| p.units[0].depends_on = vec!["U2".into()]);
        let violation = sole(&blockers(&plan));
        assert_eq!(violation.code, "unit_cycle");
        assert_eq!(violation.detail, "unit dependencies cycle through U1, U2");
    }

    #[test]
    fn a_cycle_names_its_members_and_not_the_units_waiting_on_them() {
        let plan = mutated(|p| {
            p.units[0].depends_on = vec!["U2".into()];
            p.units[2].depends_on = vec!["U1".into()];
        });
        assert_eq!(
            details(&blockers(&plan), "unit_cycle"),
            vec!["unit dependencies cycle through U1, U2"]
        );
    }

    #[test]
    fn a_dangling_dependency_is_not_mistaken_for_a_cycle() {
        let plan = mutated(|p| p.units[1].depends_on = vec!["U9".into()]);
        let violation = sole(&blockers(&plan));
        assert_eq!(violation.code, "dangling_reference");
    }

    #[test]
    fn every_text_field_the_plan_relies_on_must_hold_something() {
        let cases: [(&str, BlankMutation); 7] = [
            ("the goal statement is empty", |p, v| p.goal.statement = v),
            ("C4 has an empty question", |p, v| {
                p.decisions[3].question = v
            }),
            ("C4 alternative ALT1 has an empty value", |p, v| {
                p.decisions[3].alternatives[0].value = v
            }),
            ("R1 has an empty statement", |p, v| {
                p.requirements[0].statement = v
            }),
            ("A1 has an empty observable", |p, v| {
                p.acceptance[0].observable = v
            }),
            ("U1 has an empty title", |p, v| p.units[0].title = v),
            ("T1 has an empty command", |p, v| p.tests[0].command = v),
        ];
        for (expected, mutate) in cases {
            for blank in ["", "   ", "\t\n", "\u{00a0}", "\u{0000}", "\u{001f}"] {
                let plan = mutated(|p| mutate(p, blank.into()));
                let violation = sole(&blockers(&plan));
                assert_eq!(violation.code, "empty_field", "{expected} for {blank:?}");
                assert_eq!(violation.detail, expected, "for {blank:?}");
            }
        }
    }

    #[test]
    fn a_zero_width_character_counts_as_content() {
        let plan = mutated(|p| p.units[0].title = "\u{200b}".into());
        assert_eq!(blockers(&plan), vec![]);
    }

    #[test]
    fn a_unit_that_lists_no_paths_has_no_scope_to_change() {
        let plan = mutated(|p| p.units[0].paths.clear());
        let violation = sole(&blockers(&plan));
        assert_eq!(violation.code, "empty_field");
        assert_eq!(violation.detail, "U1 lists no paths");
    }

    #[test]
    fn a_path_that_leaves_the_repository_is_rejected() {
        let cases = [
            ("/etc/passwd", "is absolute"),
            ("/", "is absolute"),
            ("\\\\host\\share", "is absolute"),
            ("..", "escapes the repository with a `..` component"),
            ("../secrets", "escapes the repository with a `..` component"),
            (
                "src/../../etc",
                "escapes the repository with a `..` component",
            ),
            ("src/..", "escapes the repository with a `..` component"),
            (
                "src\\..\\etc",
                "escapes the repository with a `..` component",
            ),
            ("", "is empty"),
            ("   ", "is empty"),
        ];
        for (path, problem) in cases {
            let plan = mutated(|p| p.units[0].paths.push(path.into()));
            let violation = sole(&blockers(&plan));
            assert_eq!(violation.code, "empty_field", "{path:?}");
            assert_eq!(
                violation.detail,
                format!("U1 lists the path {path:?}, which {problem}")
            );
        }
    }

    #[test]
    fn a_path_that_merely_looks_like_traversal_is_allowed() {
        for path in [
            "src/..hidden/x",
            "a..b",
            "src/./x",
            "라이브러리/파일.rs",
            "src/naïve.rs",
            "x/",
        ] {
            let plan = mutated(|p| p.units[0].paths.push(path.into()));
            assert_eq!(blockers(&plan), vec![], "{path:?} should be allowed");
        }
    }

    #[test]
    fn an_open_surface_with_nothing_answered_needs_a_decision_and_a_scenario() {
        let plan = mutated(|p| {
            p.surfaces.insert("S12".into(), SurfaceStatus::Applicable);
        });
        assert_eq!(
            details(&blockers(&plan), "unanswered_surface"),
            vec![
                "S12 has no answered decision of kind decision",
                "S12 has no answered decision of kind scenario",
            ]
        );
        assert_eq!(codes(&blockers(&plan)), vec!["unanswered_surface"; 2]);
    }

    #[test]
    fn an_open_surface_without_a_scenario_is_not_covered() {
        let plan = mutated(|p| {
            p.decisions[1].kind = DecisionKind::Decision;
            reanswer(p, "C2", Selection::Recommendation);
        });
        let violation = sole(&blockers(&plan));
        assert_eq!(violation.code, "unanswered_surface");
        assert_eq!(
            violation.detail,
            "S1 has no answered decision of kind scenario"
        );
    }

    #[test]
    fn scenarios_and_terms_do_not_substitute_for_a_product_decision() {
        for kind in [DecisionKind::Scenario, DecisionKind::Term] {
            let plan = mutated(|p| {
                p.decisions[0].kind = kind;
                reanswer(p, "C1", Selection::Alternative { id: "ALT2".into() });
            });
            let violation = sole(&blockers(&plan));
            assert_eq!(violation.code, "unanswered_surface");
            assert_eq!(
                violation.detail,
                "S1 has no answered decision of kind decision"
            );
        }
    }

    #[test]
    fn assembled_plans_recheck_explanation_fields_and_source_locations() {
        for field in ["rationale", "evidence", "tradeoffs", "impact"] {
            let mut plan = fixture();
            let mut value = serde_json::to_value(&plan.decisions[0].recommendation).unwrap();
            value[field] = serde_json::json!([" \u{a0}"]);
            plan.decisions[0].recommendation = serde_json::from_value(value).unwrap();
            let errors = structural(&plan);
            assert!(
                errors
                    .iter()
                    .any(|v| v.code == "empty_field" && v.detail.contains(field)),
                "{errors:?}"
            );
        }
        for sources in [vec![], vec!["src/apply/mod.rs:12-40".into(), " ".into()]] {
            let plan = mutated(|p| p.facts[0].sources = sources);
            let violation = sole(&blockers(&plan));
            assert_eq!(violation.code, "empty_field");
            assert!(violation.detail.contains("source locations"));
        }
    }

    #[test]
    fn opening_a_closed_surface_starts_gating_the_decisions_on_it() {
        let plan = mutated(|p| {
            p.surfaces.insert("S2".into(), SurfaceStatus::Applicable);
        });
        assert_eq!(
            blockers(&plan),
            vec![
                Violation::new(
                    "orphan_decision",
                    "C3 is answered but no requirement cites it"
                ),
                Violation::new("unanswered_decision", "C4 is unanswered"),
                Violation::new(
                    "unanswered_surface",
                    "S2 has no answered decision of kind scenario"
                ),
            ]
        );
    }

    #[test]
    fn an_unanswered_decision_says_why_its_answer_does_not_count() {
        // An unanswered decision also ungrounds the requirement that cites it, so each assertion
        // names the code it is about rather than demanding it be the only one reported.
        let missing = mutated(|p| p.decisions[0].answer = None);
        assert_eq!(
            details(&blockers(&missing), "unanswered_decision"),
            vec!["C1 is unanswered".to_string()]
        );

        let reworded = mutated(|p| p.decisions[0].question = "what should C1 really do?".into());
        assert_eq!(
            details(&blockers(&reworded), "unanswered_decision"),
            vec![AnswerFreshness::StaleQuestion.explain("C1")]
        );

        let readvised = mutated(|p| p.decisions[1].recommendation = recommended("ALT2"));
        assert_eq!(
            details(&blockers(&readvised), "unanswered_decision"),
            vec![AnswerFreshness::StaleRecommendation.explain("C2")]
        );
        // The scenario answer was the surface's only one, so losing it reopens the surface too.
        assert!(codes(&blockers(&readvised)).contains(&"unanswered_surface"));
    }

    #[test]
    fn a_requirement_built_on_an_unanswered_decision_is_reported_as_ungrounded() {
        // The decision rules alone would let this through in the one case that matters most: a
        // requirement whose decision went stale still looks like a requirement, and the units under
        // it would be built from an answer the user never gave.
        let unanswered = mutated(|p| p.decisions[0].answer = None);
        assert_eq!(
            details(&blockers(&unanswered), "ungrounded_requirement"),
            vec!["R1 cites C1, which is not answered".to_string()]
        );

        let stale = mutated(|p| p.decisions[0].question = "a different question entirely".into());
        assert_eq!(
            details(&blockers(&stale), "ungrounded_requirement"),
            vec!["R1 cites C1, which is not answered".to_string()],
            "a stale answer must ground nothing, exactly as a missing one does"
        );

        // The freezable fixture grounds every requirement, so the rule is not simply always on.
        assert!(details(&blockers(&fixture()), "ungrounded_requirement").is_empty());
    }

    #[test]
    fn a_decision_on_a_closed_surface_is_not_gated_for_an_answer() {
        let plan = mutated(|p| p.decisions[2].answer = None);
        assert_eq!(blockers(&plan), vec![]);
    }

    #[test]
    fn any_open_item_blocks_the_freeze() {
        let plan = mutated(|p| {
            p.open_items.push(OpenItem {
                id: "O1".into(),
                decision_id: "C1".into(),
                detail: "needs a benchmark first".into(),
            })
        });
        let violation = sole(&blockers(&plan));
        assert_eq!(violation.code, "open_item");
        assert_eq!(
            violation.detail,
            "open item O1 on C1 is unresolved: needs a benchmark first"
        );
    }

    #[test]
    fn an_unknown_answer_with_nothing_to_resolve_it_blocks_the_freeze() {
        let plan = mutated(|p| reanswer(p, "C1", Selection::Unknown));
        let violation = sole(&blockers(&plan));
        assert_eq!(violation.code, "unresolved_unknown");
        assert_eq!(
            violation.detail,
            "C1 is answered UNKNOWN with no open item and no probe unit to resolve it"
        );
    }

    #[test]
    fn an_unknown_answer_recorded_as_an_open_item_is_reported_as_that_open_item() {
        let plan = mutated(|p| {
            reanswer(p, "C1", Selection::Unknown);
            p.open_items.push(OpenItem {
                id: "O1".into(),
                decision_id: "C1".into(),
                detail: "waiting on the platform team".into(),
            });
        });
        assert_eq!(codes(&blockers(&plan)), vec!["open_item"]);
    }

    #[test]
    fn an_unknown_answer_a_scheduled_probe_resolves_does_not_block() {
        let plan = mutated(|p| {
            p.decisions[0].recommendation = probe_required("U3");
            reanswer(p, "C1", Selection::Unknown);
        });
        assert_eq!(blockers(&plan), vec![]);
    }

    #[test]
    fn an_unknown_answer_blocks_even_on_a_surface_the_user_closed() {
        let plan = mutated(|p| reanswer(p, "C4", Selection::Unknown));
        let violation = sole(&blockers(&plan));
        assert_eq!(violation.code, "unresolved_unknown");
        assert!(
            violation.detail.starts_with("C4 is answered UNKNOWN"),
            "{}",
            violation.detail
        );
    }

    #[test]
    fn a_probe_unit_that_is_not_marked_as_a_probe_is_reported() {
        let plan = mutated(|p| {
            p.units[2].probe = false;
            // Give it a test as well, so the only rule left broken is the probe flag itself.
            p.tests.push(Test {
                id: "T3".into(),
                command: "cargo test probe::latency".into(),
                acceptance_ids: vec!["A2".into()],
                unit_id: "U3".into(),
            });
        });
        let violation = sole(&blockers(&plan));
        assert_eq!(violation.code, "probe_not_scheduled");
        assert_eq!(
            violation.detail,
            "C3 needs probe unit U3, which is not marked as a probe"
        );
    }

    #[test]
    fn an_answer_no_requirement_cites_would_be_dropped_on_the_floor() {
        let plan = mutated(|p| p.requirements[0].decision_ids = vec!["C2".into()]);
        let violation = sole(&blockers(&plan));
        assert_eq!(violation.code, "orphan_decision");
        assert_eq!(
            violation.detail,
            "C1 is answered but no requirement cites it"
        );
    }

    #[test]
    fn an_unanswered_decision_is_not_also_called_an_orphan() {
        let plan = mutated(|p| {
            p.decisions[0].answer = None;
            p.requirements[0].decision_ids = vec!["C2".into()];
        });
        assert_eq!(
            codes(&blockers(&plan)),
            vec!["unanswered_decision", "unanswered_surface"]
        );
    }

    #[test]
    fn a_requirement_no_acceptance_cites_is_reported() {
        let plan = mutated(|p| p.acceptance[0].requirement_ids = vec!["R2".into()]);
        let violation = sole(&blockers(&plan));
        assert_eq!(violation.code, "orphan_requirement");
        assert_eq!(violation.detail, "R1 is cited by no acceptance");
    }

    #[test]
    fn an_acceptance_no_unit_and_no_test_cites_is_reported_for_both() {
        let plan = mutated(|p| {
            p.acceptance.push(Acceptance {
                id: "A3".into(),
                requirement_ids: vec!["R1".into()],
                observable: "the flag is documented".into(),
            })
        });
        assert_eq!(
            blockers(&plan),
            vec![
                Violation::new("orphan_acceptance", "A3 is cited by no test"),
                Violation::new("orphan_acceptance", "A3 is cited by no unit"),
            ]
        );
    }

    #[test]
    fn an_acceptance_a_unit_builds_but_no_test_checks_is_reported() {
        let plan = mutated(|p| {
            p.acceptance.push(Acceptance {
                id: "A3".into(),
                requirement_ids: vec!["R1".into()],
                observable: "the flag is documented".into(),
            });
            p.units[0].acceptance_ids.push("A3".into());
        });
        let violation = sole(&blockers(&plan));
        assert_eq!(violation.code, "orphan_acceptance");
        assert_eq!(violation.detail, "A3 is cited by no test");
    }

    #[test]
    fn a_unit_with_no_test_cannot_be_accepted() {
        let plan = mutated(|p| {
            p.units.push(Unit {
                id: "U4".into(),
                title: "document the flag".into(),
                paths: vec!["docs/cli.md".into()],
                acceptance_ids: vec!["A1".into()],
                depends_on: vec![],
                probe: false,
            })
        });
        let violation = sole(&blockers(&plan));
        assert_eq!(violation.code, "untested_unit");
        assert_eq!(violation.detail, "U4 has no test");
    }

    #[test]
    fn a_probe_unit_needs_no_test_because_it_never_ships() {
        let plan = mutated(|p| {
            p.units.push(Unit {
                id: "U4".into(),
                title: "try the fast path".into(),
                paths: vec!["src/probe.rs".into()],
                acceptance_ids: vec!["A1".into()],
                depends_on: vec![],
                probe: true,
            })
        });
        assert_eq!(blockers(&plan), vec![]);
    }

    #[test]
    fn a_test_may_only_check_what_its_own_unit_covers() {
        let plan = mutated(|p| p.tests[0].acceptance_ids.push("A2".into()));
        let violation = sole(&blockers(&plan));
        assert_eq!(violation.code, "test_outside_unit");
        assert_eq!(
            violation.detail,
            "T1 cites acceptance A2, which its unit U1 does not cover"
        );
    }

    #[test]
    fn a_plan_with_no_full_suite_command_cannot_be_frozen() {
        for blank in ["", "   ", "\n"] {
            let plan = mutated(|p| p.full_suite = blank.into());
            let violation = sole(&blockers(&plan));
            assert_eq!(violation.code, "empty_full_suite");
            assert_eq!(violation.detail, "the full suite command is empty");
        }
    }

    #[test]
    fn each_missing_review_is_named() {
        let mut plan = fixture();
        plan.reviews.critic = None;
        let violation = sole(&blockers(&plan));
        assert_eq!(violation.code, "missing_review");
        assert_eq!(violation.detail, "the critic review has not been recorded");

        let mut plan = fixture();
        plan.reviews.cold_consumer = None;
        assert_eq!(
            sole(&blockers(&plan)).detail,
            "the cold_consumer review has not been recorded"
        );
    }

    #[test]
    fn a_review_that_did_not_pass_blocks_the_freeze() {
        let mut plan = fixture();
        plan.reviews.cold_consumer.as_mut().unwrap().passed = false;
        let violation = sole(&blockers(&plan));
        assert_eq!(violation.code, "failed_review");
        assert_eq!(violation.detail, "the cold_consumer review did not pass");
    }

    #[test]
    fn rewording_a_reviewed_decision_makes_both_reviews_stale() {
        let mut plan = fixture();
        plan.decisions[0].question = "what should C1 do about the webhook?".into();
        reanswer(
            &mut plan,
            "C1",
            Selection::Alternative { id: "ALT2".into() },
        );
        assert_eq!(
            blockers(&plan),
            vec![
                Violation::new(
                    "stale_review",
                    "the cold_consumer review covers a different plan and must be run again"
                ),
                Violation::new(
                    "stale_review",
                    "the critic review covers a different plan and must be run again"
                ),
            ]
        );
    }

    #[test]
    fn reviews_bind_to_the_review_digest_and_not_to_the_plan_digest() {
        let mut plan = fixture();
        assert_ne!(plan.digest().unwrap(), plan.review_digest().unwrap());
        let bound_to_the_wrong_digest = PlanReview {
            plan_digest: plan.digest().unwrap(),
            ts: TS.into(),
            passed: true,
            findings: vec![],
        };
        plan.reviews.cold_consumer = Some(bound_to_the_wrong_digest.clone());
        plan.reviews.critic = Some(bound_to_the_wrong_digest);
        assert_eq!(
            codes(&blockers(&plan)),
            vec!["stale_review", "stale_review"]
        );
    }

    #[test]
    fn recording_the_reviews_does_not_make_them_stale() {
        let mut plan = fixture();
        let before = plan.review_digest().unwrap();
        record_reviews(&mut plan);
        assert_eq!(plan.review_digest().unwrap(), before);
        assert_eq!(blockers(&plan), vec![]);
    }

    #[test]
    fn freezing_the_plan_does_not_reopen_the_gate() {
        let mut plan = fixture();
        let digest = plan.digest().unwrap();
        plan.frozen = Some(crate::plan::Frozen {
            digest: digest.clone(),
            confirmed_at: TS.into(),
            answer_text: format!("CONFIRM PLAN {}", digest.challenge()),
        });
        assert!(plan.is_frozen().unwrap());
        assert_eq!(blockers(&plan), vec![]);
    }

    #[test]
    fn breaking_two_rules_reports_both_of_them() {
        let mut plan = mutated(|p| p.full_suite = "  ".into());
        plan.reviews.critic = None;
        assert_eq!(
            codes(&blockers(&plan)),
            vec!["empty_full_suite", "missing_review"]
        );
    }

    #[test]
    fn violations_come_back_sorted_by_code_then_detail_without_repeats() {
        let plan = mutated(|p| {
            p.units[0].paths.push("/etc/passwd".into());
            p.units[0].paths.push("../secrets".into());
            p.acceptance.push(Acceptance {
                id: "A3".into(),
                requirement_ids: vec!["R1".into()],
                observable: "the flag is documented".into(),
            });
            p.schema = "hwahap/v2".into();
        });
        let violations = blockers(&plan);
        let mut expected = violations.clone();
        expected.sort();
        expected.dedup();
        assert_eq!(violations, expected);
        assert_eq!(
            codes(&violations),
            vec![
                "empty_field",
                "empty_field",
                "orphan_acceptance",
                "orphan_acceptance",
                "schema"
            ]
        );
    }

    #[test]
    fn the_verdict_does_not_change_when_the_plans_vectors_are_reordered() {
        let mut plan = fixture();
        plan.decisions.reverse();
        plan.requirements.reverse();
        plan.acceptance.reverse();
        plan.units.reverse();
        plan.tests.reverse();
        // Arrays are order-significant in the digest, so a reordered plan is a different plan to a
        // reviewer; only the reviews have to be recorded again.
        record_reviews(&mut plan);
        assert_eq!(structural(&plan), vec![]);
        assert_eq!(blockers(&plan), vec![]);
        assert_eq!(unit_order(&plan).unwrap(), vec!["U1", "U2", "U3"]);
    }

    #[test]
    fn reordering_alone_leaves_the_recorded_reviews_stale() {
        let mut plan = fixture();
        plan.decisions.reverse();
        assert_eq!(
            codes(&blockers(&plan)),
            vec!["stale_review", "stale_review"]
        );
    }

    #[test]
    fn the_same_failure_found_twice_is_reported_once() {
        let plan = mutated(|p| {
            p.decisions[0].depends_on = vec!["C9".into()];
            let copy = p.decisions[0].clone();
            p.decisions.push(copy);
        });
        assert_eq!(
            blockers(&plan),
            vec![
                Violation::new(
                    "dangling_reference",
                    "C1 depends on C9, which does not exist in this plan"
                ),
                Violation::new("duplicate_id", "decision id C1 appears 2 times"),
            ]
        );
    }

    #[test]
    fn a_probe_unit_that_does_not_exist_resolves_nothing() {
        let plan = mutated(|p| {
            p.decisions[3].recommendation = probe_required("U9");
            reanswer(p, "C4", Selection::Unknown);
        });
        assert_eq!(
            blockers(&plan),
            vec![
                Violation::new(
                    "dangling_reference",
                    "C4 names probe unit U9, which does not exist in this plan"
                ),
                Violation::new(
                    "unresolved_unknown",
                    "C4 is answered UNKNOWN with no open item and no probe unit to resolve it"
                ),
            ]
        );
    }

    #[test]
    fn asking_twice_gives_the_same_answer() {
        let plan = mutated(|p| p.units[0].paths.push("../secrets".into()));
        assert_eq!(blockers(&plan), blockers(&plan));
        assert_eq!(structural(&plan), structural(&plan));
    }

    #[test]
    fn the_gate_contains_every_structural_error() {
        let plan = mutated(|p| {
            p.schema = "hwahap/v2".into();
            p.units[0].paths.push("/etc".into());
        });
        let gate = blockers(&plan);
        for violation in structural(&plan) {
            assert!(
                gate.contains(&violation),
                "{violation:?} missing from {gate:?}"
            );
        }
    }

    #[test]
    fn unit_order_puts_a_dependency_before_the_unit_that_needs_it() {
        let mut plan = Plan::new("g", "main", "goal");
        plan.units = vec![
            plain_unit("U1", &["U2"]),
            plain_unit("U2", &["U3"]),
            plain_unit("U3", &[]),
        ];
        assert_eq!(unit_order(&plan).unwrap(), vec!["U3", "U2", "U1"]);
    }

    #[test]
    fn unit_order_breaks_ties_numerically_and_not_lexicographically() {
        let mut plan = Plan::new("g", "main", "goal");
        plan.units = vec![
            plain_unit("U10", &[]),
            plain_unit("U2", &[]),
            plain_unit("U1", &[]),
        ];
        assert_eq!(unit_order(&plan).unwrap(), vec!["U1", "U2", "U10"]);
    }

    #[test]
    fn unit_order_does_not_depend_on_the_order_the_units_are_stored_in() {
        let mut plan = Plan::new("g", "main", "goal");
        plan.units = vec![
            plain_unit("U1", &[]),
            plain_unit("U2", &["U1"]),
            plain_unit("U3", &["U1"]),
            plain_unit("U4", &["U2", "U3"]),
        ];
        let expected = vec!["U1", "U2", "U3", "U4"];
        assert_eq!(unit_order(&plan).unwrap(), expected);
        plan.units.reverse();
        assert_eq!(unit_order(&plan).unwrap(), expected);
        plan.units.rotate_left(2);
        assert_eq!(unit_order(&plan).unwrap(), expected);
    }

    #[test]
    fn unit_order_of_an_empty_plan_is_empty() {
        let plan = Plan::new("g", "main", "goal");
        assert_eq!(unit_order(&plan).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn unit_order_names_the_cycle_and_not_what_waits_on_it() {
        let mut plan = Plan::new("g", "main", "goal");
        plan.units = vec![
            plain_unit("U1", &["U2"]),
            plain_unit("U2", &["U1"]),
            plain_unit("U3", &["U1"]),
            plain_unit("U4", &[]),
        ];
        let error = unit_order(&plan).unwrap_err();
        assert!(
            matches!(error, Error::Rejected(_)),
            "unexpected error: {error:?}"
        );
        assert_eq!(error.to_string(), "unit dependencies cycle through U1, U2");
    }

    #[test]
    fn unit_order_refuses_a_dependency_on_a_unit_that_does_not_exist() {
        let mut plan = Plan::new("g", "main", "goal");
        plan.units = vec![plain_unit("U1", &["U9"])];
        let error = unit_order(&plan).unwrap_err();
        assert!(
            matches!(error, Error::Rejected(_)),
            "unexpected error: {error:?}"
        );
        assert_eq!(
            error.to_string(),
            "U1 depends on U9, which is not a unit in this plan"
        );
    }

    #[test]
    fn unit_order_refuses_a_duplicated_unit_id() {
        let mut plan = Plan::new("g", "main", "goal");
        plan.units = vec![plain_unit("U1", &[]), plain_unit("U1", &[])];
        let error = unit_order(&plan).unwrap_err();
        assert!(
            matches!(error, Error::Rejected(_)),
            "unexpected error: {error:?}"
        );
        assert_eq!(
            error.to_string(),
            "unit id U1 appears more than once, so there is no single order to run"
        );
    }

    #[test]
    fn unit_order_puts_a_malformed_id_last_rather_than_ahead_of_a_real_one() {
        let mut plan = Plan::new("g", "main", "goal");
        plan.units = vec![plain_unit("U01", &[]), plain_unit("U2", &[])];
        assert_eq!(unit_order(&plan).unwrap(), vec!["U2", "U01"]);
    }

    fn plain_unit(id: &str, depends_on: &[&str]) -> Unit {
        Unit {
            id: id.into(),
            title: format!("do {id}"),
            paths: vec!["src/".into()],
            acceptance_ids: vec![],
            depends_on: depends_on.iter().map(|d| (*d).to_string()).collect(),
            probe: false,
        }
    }
}
