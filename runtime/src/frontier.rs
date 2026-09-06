//! Which questions can be asked right now.
//!
//! PLAN puts the whole decision frontier to the user in one round instead of one question at a
//! time: every decision whose prerequisites are already answered and which is not itself answered.
//! Answering creates new implications, so the frontier is recomputed from the plan each round until
//! it is empty. It is derived and never stored, because a stored frontier could disagree with the
//! plan it came from, and the plan is the only thing anyone is allowed to act on.
//!
//! A prerequisite that cannot be satisfied — it names a decision that does not exist, one the user
//! closed with `S<n>=NA`, or one caught in a cycle — leaves its dependant waiting rather than
//! quietly ready. That is deliberate: guessing would put a question to the user that the plan does
//! not actually support. [`prerequisite_cycles`] and [`dangling_prerequisites`] name those defects,
//! because the frontier alone only shows the symptom.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::{Error, Result};
use crate::plan::{AnswerFreshness, Decision, Plan, Surface};

/// A decision that cannot be asked yet, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocked {
    /// The `C<n>` that must wait.
    pub id: String,
    /// The unanswered prerequisites, sorted.
    pub waiting_on: Vec<String>,
}

/// A decision whose recorded answer no longer counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stale {
    /// The `C<n>` whose answer stopped counting.
    pub id: String,
    /// What moved out from under the answer.
    pub reason: crate::plan::AnswerFreshness,
}

/// Everything one PLAN round needs: what to ask, what still waits, and what came undone.
///
/// Every list is ordered by decision number — `C2` before `C10`, ids that are not `C<n>` after
/// both — so the same plan always reads back in the same order whatever order it is stored in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frontier {
    /// Decision ids that should be put to the user now.
    pub ready: Vec<String>,
    /// Decisions that cannot be asked yet.
    pub blocked: Vec<Blocked>,
    /// Decisions that were answered but went stale; they are also in `ready`, unless their own
    /// prerequisites came unanswered too, which puts them in `blocked` instead.
    pub stale: Vec<Stale>,
}

impl Frontier {
    /// True when there is nothing left to ask and nothing left waiting.
    ///
    /// `stale` is not consulted: a stale decision is by definition unanswered, so it is already
    /// counted in `ready` or `blocked`.
    pub fn is_empty(&self) -> bool {
        self.ready.is_empty() && self.blocked.is_empty()
    }
}

/// Derives the frontier over the applicable surfaces only.
///
/// A decision on a surface the user marked `S<n>=NA` is invisible: the user removed the question
/// rather than answering it, so it is neither asked nor able to satisfy a dependant.
///
/// Mutually dependent decisions block each other, so a cycle whose members are all unanswered is
/// never ready — but a cycle one of whose members is already answered leaves the rest ready, and
/// that is deliberate. An adjust round can add an edge to a question that was answered before the
/// edge existed ([`Decision::identity_digest`] does not cover `depends_on`), and the alternative
/// would be a `blocked` entry that either waits on nothing or names a prerequisite the decision
/// does not have. [`prerequisite_cycles`] still reports the cycle as the defect it is.
pub fn derive(plan: &Plan) -> Result<Frontier> {
    reject_duplicate_ids(plan)?;
    require_declared_surfaces(plan)?;
    let applicable: BTreeSet<_> = plan.applicable_surfaces().into_iter().collect();
    let asked: Vec<&Decision> = plan
        .decisions
        .iter()
        .filter(|d| applicable.contains(&d.surface))
        .collect();

    // `Fresh` is exactly what `Decision::is_answered` reports. It is computed once per decision
    // because each call hashes the question and its alternatives, and every decision is consulted
    // twice: once as a prerequisite, once as a question of its own.
    let mut freshness = Vec::with_capacity(asked.len());
    for decision in &asked {
        freshness.push(decision.answer_freshness()?);
    }
    let satisfied: BTreeSet<&str> = asked
        .iter()
        .zip(&freshness)
        .filter(|(_, freshness)| matches!(freshness, AnswerFreshness::Fresh))
        .map(|(decision, _)| decision.id.as_str())
        .collect();

    let mut frontier = Frontier {
        ready: Vec::new(),
        blocked: Vec::new(),
        stale: Vec::new(),
    };
    for (decision, freshness) in asked.iter().zip(&freshness) {
        match freshness {
            AnswerFreshness::Fresh => continue,
            AnswerFreshness::Missing => {}
            reason => frontier.stale.push(Stale {
                id: decision.id.clone(),
                reason: *reason,
            }),
        }
        // Only a fresh answer on an applicable surface satisfies a prerequisite, so an unknown id,
        // an `S<n>=NA` question and a mutually dependent pair all read the same way here:
        // unsatisfied. Nothing follows an edge, so cyclic input terminates like any other.
        let waiting: BTreeSet<&str> = decision
            .depends_on
            .iter()
            .map(String::as_str)
            .filter(|prerequisite| !satisfied.contains(prerequisite))
            .collect();
        if waiting.is_empty() {
            frontier.ready.push(decision.id.clone());
        } else {
            let mut waiting_on: Vec<String> = waiting.into_iter().map(str::to_string).collect();
            sort_ids(&mut waiting_on);
            frontier.blocked.push(Blocked {
                id: decision.id.clone(),
                waiting_on,
            });
        }
    }

    sort_ids(&mut frontier.ready);
    frontier
        .blocked
        .sort_by(|a, b| order_key(&a.id).cmp(&order_key(&b.id)));
    frontier
        .stale
        .sort_by(|a, b| order_key(&a.id).cmp(&order_key(&b.id)));
    Ok(frontier)
}

/// Every prerequisite cycle among decisions, each reported once with its members sorted.
/// The outer Vec is sorted too, so the output is deterministic.
///
/// Surfaces are ignored: a cycle is a structural defect of the plan, and marking a surface `NA`
/// hides the questions without repairing a graph the user may re-open next round.
pub fn prerequisite_cycles(plan: &Plan) -> Vec<Vec<String>> {
    let (ids, edges) = dependency_graph(plan);
    let mut cycles: Vec<Vec<String>> = strongly_connected(&edges)
        .into_iter()
        // A lone decision is a cycle only when it names itself; every larger group is one by
        // construction, because each member is reachable from every other.
        .filter(|group| group.len() > 1 || group.first().is_some_and(|&v| edges[v].contains(&v)))
        .map(|group| {
            let mut members: Vec<String> = group.into_iter().map(|v| ids[v].to_string()).collect();
            sort_ids(&mut members);
            members
        })
        .collect();
    cycles.sort_by(|a, b| {
        a.iter()
            .map(|id| order_key(id))
            .cmp(b.iter().map(|id| order_key(id)))
    });
    cycles
}

/// Prerequisite ids that name a decision that does not exist, sorted and deduplicated,
/// reported as "C7 -> C99".
pub fn dangling_prerequisites(plan: &Plan) -> Vec<String> {
    let known: BTreeSet<&str> = plan.decisions.iter().map(|d| d.id.as_str()).collect();
    let mut dangling: BTreeSet<(&str, &str)> = BTreeSet::new();
    for decision in &plan.decisions {
        for prerequisite in &decision.depends_on {
            if !known.contains(prerequisite.as_str()) {
                dangling.insert((decision.id.as_str(), prerequisite.as_str()));
            }
        }
    }
    let mut pairs: Vec<(&str, &str)> = dangling.into_iter().collect();
    pairs.sort_by(|a, b| (order_key(a.0), order_key(a.1)).cmp(&(order_key(b.0), order_key(b.1))));
    pairs
        .into_iter()
        .map(|(from, to)| format!("{from} -> {to}"))
        .collect()
}

/// Rejects a plan that answers "is `C7` answered?" in two different ways.
///
/// Every decision is checked, not just the applicable ones, because a prerequisite naming a
/// duplicated id has no single answer whatever surface the twin sits on.
fn reject_duplicate_ids(plan: &Plan) -> Result<()> {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for decision in &plan.decisions {
        if !seen.insert(decision.id.as_str()) {
            return Err(Error::Corrupt(format!(
                "two decisions share the id {:?}; a prerequisite naming it has no single answer",
                decision.id
            )));
        }
    }
    Ok(())
}

/// Rejects a plan that carries a decision on a surface it never declares.
///
/// [`Plan::applicable_surfaces`] reports an absent surface exactly like one the user closed with
/// `S<n>=NA`, so without this the frontier would quietly drop the question and PLAN could freeze a
/// decision the user was never asked. Only the surfaces decisions actually sit on are checked:
/// whether an unused surface is declared decides nothing this round.
fn require_declared_surfaces(plan: &Plan) -> Result<()> {
    let undeclared: BTreeSet<Surface> = plan
        .decisions
        .iter()
        .map(|decision| decision.surface)
        .filter(|surface| !plan.surfaces.contains_key(surface.id()))
        .collect();
    // Sorted, so which surface the message names never depends on the order of `decisions`.
    if let Some(surface) = undeclared.into_iter().next() {
        return Err(Error::Corrupt(format!(
            "a decision sits on {surface}, but the plan never says whether {surface} applies; \
             an absent surface is not one the user closed with {surface}=NA"
        )));
    }
    Ok(())
}

/// The decision graph as indices.
///
/// The ordered maps are what make the traversal reproducible, and they also fold two decisions that
/// share an id into one node: these checks run on plans nobody has accepted yet, so a duplicated id
/// must not let half its prerequisites escape inspection.
///
/// Prerequisites that name no decision are dropped: they can never be part of a cycle, and
/// [`dangling_prerequisites`] reports them instead.
fn dependency_graph(plan: &Plan) -> (Vec<&str>, Vec<Vec<usize>>) {
    let mut prerequisites: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for decision in &plan.decisions {
        prerequisites
            .entry(decision.id.as_str())
            .or_default()
            .extend(decision.depends_on.iter().map(String::as_str));
    }
    let ids: Vec<&str> = prerequisites.keys().copied().collect();
    let position: BTreeMap<&str, usize> = ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    // `keys` and `values` walk the same order, so `edges[i]` is the row belonging to `ids[i]`,
    // and no lookup can fail.
    let edges: Vec<Vec<usize>> = prerequisites
        .values()
        .map(|deps| {
            deps.iter()
                .filter_map(|p| position.get(p).copied())
                .collect()
        })
        .collect();
    (ids, edges)
}

/// Tarjan's strongly connected components, over an explicit stack.
///
/// Iterative rather than recursive: a plan may chain hundreds of decisions, and one that overflowed
/// the stack would abort the process instead of reporting the defect that caused it.
fn strongly_connected(edges: &[Vec<usize>]) -> Vec<Vec<usize>> {
    const UNVISITED: usize = usize::MAX;
    let count = edges.len();
    let mut index = vec![UNVISITED; count];
    let mut lowlink = vec![0usize; count];
    let mut on_stack = vec![false; count];
    let mut stack: Vec<usize> = Vec::new();
    let mut next_index = 0usize;
    let mut components: Vec<Vec<usize>> = Vec::new();

    for start in 0..count {
        if index[start] != UNVISITED {
            continue;
        }
        // Each entry is a node plus the neighbour to resume at, standing in for a stack frame.
        let mut work: Vec<(usize, usize)> = vec![(start, 0)];
        while let Some((node, resume_at)) = work.pop() {
            if resume_at == 0 {
                index[node] = next_index;
                lowlink[node] = next_index;
                next_index += 1;
                stack.push(node);
                on_stack[node] = true;
            }
            let mut descended = false;
            for (offset, &next) in edges[node].iter().enumerate().skip(resume_at) {
                if index[next] == UNVISITED {
                    work.push((node, offset + 1));
                    work.push((next, 0));
                    descended = true;
                    break;
                } else if on_stack[next] {
                    lowlink[node] = lowlink[node].min(index[next]);
                }
            }
            if descended {
                continue;
            }
            if lowlink[node] == index[node] {
                let mut component = Vec::new();
                while let Some(member) = stack.pop() {
                    on_stack[member] = false;
                    component.push(member);
                    if member == node {
                        break;
                    }
                }
                components.push(component);
            }
            // The parent frame is whatever is left on top, so returning to it is a fold, not a
            // second traversal.
            if let Some(&(parent, _)) = work.last() {
                lowlink[parent] = lowlink[parent].min(lowlink[node]);
            }
        }
    }
    components
}

/// Sorts decision ids the way the user reads them: `C2` before `C10`.
fn sort_ids(ids: &mut [String]) {
    ids.sort_by(|a, b| order_key(a).cmp(&order_key(b)));
}

/// Orders `C<n>` by `n`, and everything else after it by its raw text.
///
/// The leading flag keeps the order total even when a plan carries an id this crate never writes,
/// so a malformed id changes where a question appears but never whether it appears.
fn order_key(id: &str) -> (u8, u64, &str) {
    match decision_number(id) {
        Some(n) => (0, n, id),
        None => (1, 0, id),
    }
}

/// The `n` in `C<n>`, for ids in exactly the form this crate writes.
///
/// Deliberately stricter than `str::parse`, which accepts `C+7` and `C007` and would sort them as
/// if they were `C7`. Two ids that sort as one number are two questions that can hide behind each
/// other, so anything but the canonical form is treated as unnumbered instead.
fn decision_number(id: &str) -> Option<u64> {
    let digits = id.strip_prefix('C')?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if digits.len() > 1 && digits.starts_with('0') {
        return None;
    }
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::Digest;
    use crate::plan::{
        Alternative, Answer, Confidence, DecisionKind, Recommendation, Selection, Surface,
        SurfaceStatus,
    };

    fn decision(id: &str, depends_on: &[&str]) -> Decision {
        Decision {
            id: id.into(),
            surface: Surface::S1,
            kind: DecisionKind::Decision,
            question: format!("what should {id} do?"),
            alternatives: vec![
                Alternative {
                    id: "ALT1".into(),
                    value: "one".into(),
                },
                Alternative {
                    id: "ALT2".into(),
                    value: "two".into(),
                },
            ],
            recommendation: Recommendation::Recommended {
                choice: "ALT1".into(),
                rationale: vec!["it matches the existing path".into()],
                evidence: vec!["F1".into()],
                tradeoffs: vec!["one more branch".into()],
                impact: vec!["api".into()],
                confidence: Confidence::High,
            },
            depends_on: depends_on.iter().map(|p| (*p).to_string()).collect(),
            answer: None,
        }
    }

    fn record_answer(target: &mut Decision, selection: Selection) {
        let identity = target.identity_digest().unwrap();
        let recommendation = matches!(selection, Selection::Recommendation)
            .then(|| target.recommendation_digest().unwrap());
        let text = format!("{}=REC", target.id);
        target.answer = Some(Answer {
            text,
            selection,
            ts: "2026-09-04T00:00:00Z".into(),
            identity,
            recommendation,
        });
    }

    fn answered(id: &str, depends_on: &[&str]) -> Decision {
        let mut target = decision(id, depends_on);
        record_answer(&mut target, Selection::Recommendation);
        target
    }

    fn plan_of(decisions: Vec<Decision>) -> Plan {
        let mut plan = Plan::new("2026-09-04-frontier", "main", "derive the frontier");
        plan.decisions = decisions;
        plan
    }

    fn mark_not_applicable(plan: &mut Plan, surface: Surface) {
        plan.surfaces.insert(
            surface.id().into(),
            SurfaceStatus::NotApplicable {
                reason: "no released consumers".into(),
                answer: Answer {
                    text: format!("{}=NA", surface.id()),
                    selection: Selection::NotApplicable,
                    ts: "2026-09-04T00:00:00Z".into(),
                    identity: Digest::zero(),
                    recommendation: None,
                },
            },
        );
    }

    fn blocked(id: &str, waiting_on: &[&str]) -> Blocked {
        Blocked {
            id: id.into(),
            waiting_on: waiting_on.iter().map(|w| (*w).to_string()).collect(),
        }
    }

    fn stale(id: &str, reason: AnswerFreshness) -> Stale {
        Stale {
            id: id.into(),
            reason,
        }
    }

    fn ready_of(decisions: Vec<Decision>) -> Vec<String> {
        derive(&plan_of(decisions)).unwrap().ready
    }

    #[test]
    fn an_empty_plan_yields_a_frontier_that_is_empty() {
        let frontier = derive(&Plan::new("g", "main", "goal")).unwrap();
        assert!(frontier.ready.is_empty());
        assert!(frontier.blocked.is_empty());
        assert!(frontier.stale.is_empty());
        assert!(frontier.is_empty());
    }

    #[test]
    fn a_decision_with_no_prerequisites_is_ready_immediately() {
        let frontier = derive(&plan_of(vec![decision("C1", &[])])).unwrap();
        assert_eq!(frontier.ready, ["C1"]);
        assert!(frontier.blocked.is_empty());
        assert!(frontier.stale.is_empty());
        assert!(!frontier.is_empty());
    }

    #[test]
    fn a_freshly_answered_decision_leaves_the_frontier() {
        let frontier = derive(&plan_of(vec![answered("C1", &[])])).unwrap();
        assert!(frontier.is_empty());
        assert!(frontier.stale.is_empty());
    }

    #[test]
    fn a_chain_asks_exactly_one_question_at_a_time() {
        let mut plan = plan_of(vec![
            decision("C1", &[]),
            decision("C2", &["C1"]),
            decision("C3", &["C2"]),
        ]);
        let first = derive(&plan).unwrap();
        assert_eq!(first.ready, ["C1"]);
        assert_eq!(
            first.blocked,
            [blocked("C2", &["C1"]), blocked("C3", &["C2"])]
        );

        record_answer(plan.decision_mut("C1").unwrap(), Selection::Recommendation);
        let second = derive(&plan).unwrap();
        assert_eq!(second.ready, ["C2"]);
        assert_eq!(second.blocked, [blocked("C3", &["C2"])]);

        record_answer(plan.decision_mut("C2").unwrap(), Selection::Recommendation);
        let third = derive(&plan).unwrap();
        assert_eq!(third.ready, ["C3"]);
        assert!(third.blocked.is_empty());

        record_answer(plan.decision_mut("C3").unwrap(), Selection::Recommendation);
        assert!(derive(&plan).unwrap().is_empty());
    }

    #[test]
    fn a_diamond_asks_both_middle_decisions_in_the_same_round() {
        let mut plan = plan_of(vec![
            answered("C1", &[]),
            decision("C2", &["C1"]),
            decision("C3", &["C1"]),
            decision("C4", &["C2", "C3"]),
        ]);
        let first = derive(&plan).unwrap();
        assert_eq!(first.ready, ["C2", "C3"]);
        assert_eq!(first.blocked, [blocked("C4", &["C2", "C3"])]);

        record_answer(plan.decision_mut("C2").unwrap(), Selection::Recommendation);
        let second = derive(&plan).unwrap();
        assert_eq!(second.ready, ["C3"]);
        assert_eq!(second.blocked, [blocked("C4", &["C3"])]);

        record_answer(plan.decision_mut("C3").unwrap(), Selection::Recommendation);
        let third = derive(&plan).unwrap();
        assert_eq!(third.ready, ["C4"]);
        assert!(third.blocked.is_empty());
    }

    #[test]
    fn ready_is_ordered_by_number_so_c2_comes_before_c10() {
        let ready = ready_of(vec![
            decision("C10", &[]),
            decision("C2", &[]),
            decision("C1", &[]),
        ]);
        assert_eq!(ready, ["C1", "C2", "C10"]);
    }

    #[test]
    fn ids_that_are_not_c_numbers_sort_after_the_numbered_ones() {
        let ready = ready_of(vec![
            decision("banana", &[]),
            decision("Czz", &[]),
            decision("C", &[]),
            decision("C10", &[]),
            decision("C2", &[]),
        ]);
        assert_eq!(ready, ["C2", "C10", "C", "Czz", "banana"]);
    }

    #[test]
    fn zero_padded_signed_and_spaced_ids_do_not_pass_as_numbers() {
        let ready = ready_of(vec![
            decision("C01", &[]),
            decision("C+2", &[]),
            decision("C 3", &[]),
            decision("C1", &[]),
        ]);
        // Any of these parsing as a number would sort it among the numbered ids instead of after
        // them by raw text.
        assert_eq!(ready, ["C1", "C 3", "C+2", "C01"]);
    }

    #[test]
    fn a_non_ascii_digit_id_is_not_a_number() {
        let ready = ready_of(vec![
            decision("C\u{0663}", &[]),
            decision("C10", &[]),
            decision("C1", &[]),
        ]);
        // Arabic-Indic three would sort between C1 and C10 if it counted as 3.
        assert_eq!(ready, ["C1", "C10", "C\u{0663}"]);
    }

    #[test]
    fn an_id_whose_number_overflows_u64_sorts_after_the_real_ones() {
        let huge = format!("C{}", "9".repeat(25));
        let ready = ready_of(vec![
            decision(&huge, &[]),
            decision("C10", &[]),
            decision("C1", &[]),
        ]);
        assert_eq!(ready, ["C1", "C10", huge.as_str()]);
    }

    #[test]
    fn an_unanswered_prerequisite_blocks_its_dependant() {
        let frontier =
            derive(&plan_of(vec![decision("C1", &[]), decision("C2", &["C1"])])).unwrap();
        assert_eq!(frontier.ready, ["C1"]);
        assert_eq!(frontier.blocked, [blocked("C2", &["C1"])]);
        assert!(!frontier.is_empty());
    }

    #[test]
    fn waiting_on_names_only_the_unanswered_prerequisites() {
        let frontier = derive(&plan_of(vec![
            answered("C1", &[]),
            decision("C2", &[]),
            decision("C3", &["C1", "C2"]),
        ]))
        .unwrap();
        assert_eq!(frontier.blocked, [blocked("C3", &["C2"])]);
    }

    #[test]
    fn waiting_on_is_deduplicated_and_ordered_by_number() {
        let frontier = derive(&plan_of(vec![
            decision("C2", &[]),
            decision("C10", &[]),
            decision("C11", &["C10", "C2", "C10", "C2"]),
        ]))
        .unwrap();
        assert_eq!(frontier.blocked, [blocked("C11", &["C2", "C10"])]);
    }

    #[test]
    fn blocked_entries_are_ordered_by_number() {
        let frontier = derive(&plan_of(vec![
            decision("C10", &["C1"]),
            decision("C2", &["C1"]),
            decision("C1", &[]),
        ]))
        .unwrap();
        assert_eq!(
            frontier.blocked,
            [blocked("C2", &["C1"]), blocked("C10", &["C1"])]
        );
    }

    #[test]
    fn answering_then_rewording_a_question_makes_it_ready_and_stale_and_blocks_downstream() {
        let mut plan = plan_of(vec![answered("C1", &[]), decision("C2", &["C1"])]);
        assert_eq!(derive(&plan).unwrap().ready, ["C2"]);

        plan.decision_mut("C1").unwrap().question = "what should C1 do, exactly?".into();
        let frontier = derive(&plan).unwrap();
        assert_eq!(frontier.ready, ["C1"]);
        assert_eq!(
            frontier.stale,
            [stale("C1", AnswerFreshness::StaleQuestion)]
        );
        assert_eq!(frontier.blocked, [blocked("C2", &["C1"])]);
    }

    #[test]
    fn rewording_a_question_into_another_script_is_still_a_reword() {
        let mut plan = plan_of(vec![answered("C1", &[]), decision("C2", &["C1"])]);
        plan.decision_mut("C1").unwrap().question = "정책을 언제 적용할까요? 👩‍👩‍👧".into();
        let frontier = derive(&plan).unwrap();
        assert_eq!(frontier.ready, ["C1"]);
        assert_eq!(
            frontier.stale,
            [stale("C1", AnswerFreshness::StaleQuestion)]
        );
        assert_eq!(frontier.blocked, [blocked("C2", &["C1"])]);
    }

    #[test]
    fn a_changed_recommendation_returns_a_rec_answer_to_the_frontier_as_stale() {
        let mut plan = plan_of(vec![answered("C1", &[]), decision("C2", &["C1"])]);
        plan.decision_mut("C1").unwrap().recommendation = Recommendation::NoRecommendation {
            rationale: vec!["the two options are a genuine tie".into()],
        };
        let frontier = derive(&plan).unwrap();
        assert_eq!(frontier.ready, ["C1"]);
        assert_eq!(
            frontier.stale,
            [stale("C1", AnswerFreshness::StaleRecommendation)]
        );
        assert_eq!(frontier.blocked, [blocked("C2", &["C1"])]);
    }

    #[test]
    fn stale_entries_are_ordered_by_number() {
        let mut ten = answered("C10", &[]);
        ten.question = "reworded after it was answered".into();
        let mut two = answered("C2", &[]);
        two.recommendation = Recommendation::NoRecommendation {
            rationale: vec!["the two options are a genuine tie".into()],
        };
        let frontier = derive(&plan_of(vec![ten, two])).unwrap();
        assert_eq!(
            frontier.stale,
            [
                stale("C2", AnswerFreshness::StaleRecommendation),
                stale("C10", AnswerFreshness::StaleQuestion),
            ]
        );
        assert_eq!(frontier.ready, ["C2", "C10"]);
    }

    #[test]
    fn a_never_answered_decision_is_ready_without_being_stale() {
        let frontier = derive(&plan_of(vec![decision("C1", &[])])).unwrap();
        assert_eq!(frontier.ready, ["C1"]);
        assert!(
            frontier.stale.is_empty(),
            "a question that was never answered cannot have gone stale"
        );
    }

    #[test]
    fn a_stale_decision_with_unanswered_prerequisites_is_blocked_rather_than_ready() {
        let mut reworded = answered("C2", &["C1"]);
        reworded.question = "reworded after it was answered".into();
        let frontier = derive(&plan_of(vec![decision("C1", &[]), reworded])).unwrap();
        assert_eq!(frontier.ready, ["C1"]);
        assert_eq!(frontier.blocked, [blocked("C2", &["C1"])]);
        assert_eq!(
            frontier.stale,
            [stale("C2", AnswerFreshness::StaleQuestion)]
        );
    }

    #[test]
    fn a_decision_on_a_not_applicable_surface_is_invisible_to_the_frontier() {
        let mut hidden = decision("C9", &[]);
        hidden.surface = Surface::S9;
        let mut plan = plan_of(vec![decision("C1", &[]), hidden]);
        assert_eq!(derive(&plan).unwrap().ready, ["C1", "C9"]);

        mark_not_applicable(&mut plan, Surface::S9);
        let frontier = derive(&plan).unwrap();
        assert_eq!(frontier.ready, ["C1"]);
        assert!(frontier.blocked.is_empty());
    }

    #[test]
    fn a_prerequisite_on_a_not_applicable_surface_blocks_even_when_it_is_answered() {
        let mut removed = decision("C9", &[]);
        // The surface is part of a decision's identity, so it has to be set before the answer is
        // recorded or the answer would be stale for a different reason than this test is about.
        removed.surface = Surface::S9;
        record_answer(&mut removed, Selection::Recommendation);
        let mut plan = plan_of(vec![removed, decision("C1", &["C9"])]);
        assert_eq!(derive(&plan).unwrap().ready, ["C1"]);

        mark_not_applicable(&mut plan, Surface::S9);
        let frontier = derive(&plan).unwrap();
        assert!(frontier.ready.is_empty());
        assert_eq!(frontier.blocked, [blocked("C1", &["C9"])]);
    }

    #[test]
    fn a_prerequisite_that_names_no_decision_blocks_forever() {
        let plan = plan_of(vec![decision("C7", &["C99"])]);
        let frontier = derive(&plan).unwrap();
        assert!(frontier.ready.is_empty());
        assert_eq!(frontier.blocked, [blocked("C7", &["C99"])]);
        assert!(!frontier.is_empty());
        assert_eq!(dangling_prerequisites(&plan), ["C7 -> C99"]);
    }

    #[test]
    fn two_decisions_that_depend_on_each_other_are_both_blocked() {
        let plan = plan_of(vec![decision("C1", &["C2"]), decision("C2", &["C1"])]);
        let frontier = derive(&plan).unwrap();
        assert!(frontier.ready.is_empty());
        assert_eq!(
            frontier.blocked,
            [blocked("C1", &["C2"]), blocked("C2", &["C1"])]
        );
        assert_eq!(prerequisite_cycles(&plan), [["C1", "C2"]]);
    }

    #[test]
    fn a_decision_that_depends_on_itself_is_blocked_on_itself() {
        let frontier = derive(&plan_of(vec![decision("C3", &["C3"])])).unwrap();
        assert!(frontier.ready.is_empty());
        assert_eq!(frontier.blocked, [blocked("C3", &["C3"])]);
    }

    #[test]
    fn a_cycle_member_that_is_already_answered_does_not_block_its_partner() {
        // An adjust round can add a prerequisite to a question that was answered before the edge
        // existed. Asking the other member is then the only way out, so it stays ready; the cycle
        // is still reported as the defect it is.
        let plan = plan_of(vec![answered("C1", &["C2"]), decision("C2", &["C1"])]);
        let frontier = derive(&plan).unwrap();
        assert_eq!(frontier.ready, ["C2"]);
        assert!(frontier.blocked.is_empty());
        assert_eq!(prerequisite_cycles(&plan), [["C1", "C2"]]);
    }

    #[test]
    fn an_unknown_answer_counts_as_answered_and_unblocks_its_dependants() {
        let mut open = decision("C1", &[]);
        record_answer(&mut open, Selection::Unknown);
        let frontier = derive(&plan_of(vec![open, decision("C2", &["C1"])])).unwrap();
        assert_eq!(frontier.ready, ["C2"]);
        assert!(frontier.blocked.is_empty());
    }

    #[test]
    fn two_decisions_sharing_an_id_are_corrupt_rather_than_ambiguous() {
        let plan = plan_of(vec![
            answered("C1", &[]),
            decision("C1", &[]),
            decision("C2", &["C1"]),
        ]);
        let err = derive(&plan).unwrap_err();
        assert!(
            matches!(err, Error::Corrupt(_)),
            "unexpected error: {err:?}"
        );
        let message = err.to_string();
        assert!(
            message.contains("two decisions share the id \"C1\""),
            "{message}"
        );
        assert!(message.contains("no single answer"), "{message}");
    }

    #[test]
    fn is_empty_counts_ready_and_blocked_and_ignores_stale() {
        let nothing = Frontier {
            ready: vec![],
            blocked: vec![],
            stale: vec![],
        };
        assert!(nothing.is_empty());
        let stale_only = Frontier {
            ready: vec![],
            blocked: vec![],
            stale: vec![stale("C1", AnswerFreshness::StaleQuestion)],
        };
        assert!(stale_only.is_empty());
        let ready_only = Frontier {
            ready: vec!["C1".into()],
            blocked: vec![],
            stale: vec![],
        };
        assert!(!ready_only.is_empty());
        let blocked_only = Frontier {
            ready: vec![],
            blocked: vec![blocked("C2", &["C1"])],
            stale: vec![],
        };
        assert!(!blocked_only.is_empty());
    }

    #[test]
    fn deriving_twice_yields_the_same_frontier_whatever_order_the_decisions_are_stored_in() {
        let mut reworded = answered("C4", &[]);
        reworded.question = "changed after the answer".into();
        let plan = plan_of(vec![
            answered("C1", &[]),
            decision("C3", &["C2"]),
            decision("C2", &["C1"]),
            reworded,
        ]);
        let frontier = derive(&plan).unwrap();
        assert_eq!(frontier.ready, ["C2", "C4"]);
        assert_eq!(frontier.blocked, [blocked("C3", &["C2"])]);
        assert_eq!(
            frontier.stale,
            [stale("C4", AnswerFreshness::StaleQuestion)]
        );

        assert_eq!(derive(&plan).unwrap(), frontier);
        let mut reversed = plan.clone();
        reversed.decisions.reverse();
        assert_eq!(derive(&reversed).unwrap(), frontier);
    }

    #[test]
    fn ids_are_compared_byte_for_byte_including_control_characters() {
        let plan = plan_of(vec![
            answered("C1", &[]),
            decision("C2", &["C1\n"]),
            decision("C3", &["C1\u{0}"]),
        ]);
        let frontier = derive(&plan).unwrap();
        assert!(frontier.ready.is_empty());
        assert_eq!(
            frontier.blocked,
            [blocked("C2", &["C1\n"]), blocked("C3", &["C1\u{0}"])]
        );
        assert_eq!(
            dangling_prerequisites(&plan),
            ["C2 -> C1\n", "C3 -> C1\u{0}"]
        );
    }

    #[test]
    fn a_whitespace_only_id_participates_and_sorts_after_the_numbered_ones() {
        let plan = plan_of(vec![decision("   ", &[]), decision("C1", &["   "])]);
        let frontier = derive(&plan).unwrap();
        assert_eq!(frontier.ready, ["   "]);
        assert_eq!(frontier.blocked, [blocked("C1", &["   "])]);
        assert!(dangling_prerequisites(&plan).is_empty());
    }

    #[test]
    fn two_hundred_chained_decisions_do_not_blow_the_stack() {
        let mut decisions = vec![decision("C1", &[])];
        for n in 2..=200u32 {
            decisions.push(decision(&format!("C{n}"), &[&format!("C{}", n - 1)]));
        }
        let plan = plan_of(decisions);
        let frontier = derive(&plan).unwrap();
        assert_eq!(frontier.ready, ["C1"]);
        assert_eq!(frontier.blocked.len(), 199);
        assert_eq!(frontier.blocked[0], blocked("C2", &["C1"]));
        assert_eq!(frontier.blocked[198], blocked("C200", &["C199"]));
        assert!(prerequisite_cycles(&plan).is_empty());
        assert!(dangling_prerequisites(&plan).is_empty());
    }

    #[test]
    fn a_two_hundred_member_cycle_is_found_without_recursion() {
        let mut decisions = vec![decision("C1", &["C200"])];
        for n in 2..=200u32 {
            decisions.push(decision(&format!("C{n}"), &[&format!("C{}", n - 1)]));
        }
        let cycles = prerequisite_cycles(&plan_of(decisions));
        assert_eq!(cycles.len(), 1);
        assert_eq!(cycles[0].len(), 200);
        assert_eq!(cycles[0][0], "C1");
        assert_eq!(cycles[0][199], "C200");
    }

    #[test]
    fn a_dag_has_no_prerequisite_cycles() {
        let plan = plan_of(vec![
            decision("C1", &[]),
            decision("C2", &["C1"]),
            decision("C3", &["C1"]),
            decision("C4", &["C2", "C3"]),
        ]);
        assert!(prerequisite_cycles(&plan).is_empty());
    }

    #[test]
    fn a_three_cycle_is_reported_as_one_group() {
        let plan = plan_of(vec![
            decision("C1", &["C3"]),
            decision("C2", &["C1"]),
            decision("C3", &["C2"]),
        ]);
        assert_eq!(prerequisite_cycles(&plan), [["C1", "C2", "C3"]]);
    }

    #[test]
    fn a_self_dependency_is_a_cycle_of_length_one() {
        let plan = plan_of(vec![decision("C1", &[]), decision("C3", &["C3"])]);
        assert_eq!(prerequisite_cycles(&plan), [["C3"]]);
    }

    #[test]
    fn two_disjoint_cycles_are_reported_separately_in_number_order() {
        let plan = plan_of(vec![
            decision("C10", &["C11"]),
            decision("C11", &["C10"]),
            decision("C2", &["C3"]),
            decision("C3", &["C2"]),
            decision("C1", &[]),
        ]);
        assert_eq!(prerequisite_cycles(&plan), [["C2", "C3"], ["C10", "C11"]]);
    }

    #[test]
    fn members_of_a_cycle_are_sorted_by_number_not_by_text() {
        let plan = plan_of(vec![decision("C10", &["C2"]), decision("C2", &["C10"])]);
        assert_eq!(prerequisite_cycles(&plan), [["C2", "C10"]]);
    }

    #[test]
    fn a_decision_pointing_into_a_cycle_is_not_part_of_it() {
        let plan = plan_of(vec![
            decision("C1", &["C2"]),
            decision("C2", &["C3"]),
            decision("C3", &["C2"]),
        ]);
        assert_eq!(prerequisite_cycles(&plan), [["C2", "C3"]]);
    }

    #[test]
    fn a_cycle_that_also_points_at_an_earlier_cycle_is_still_found() {
        // C3 reaches a component that is already closed. Folding that edge into C3's lowlink
        // instead of ignoring it would swallow C3 and C4's own cycle entirely.
        let plan = plan_of(vec![
            decision("C1", &["C2"]),
            decision("C2", &["C1"]),
            decision("C3", &["C1", "C4"]),
            decision("C4", &["C3"]),
        ]);
        assert_eq!(prerequisite_cycles(&plan), [["C1", "C2"], ["C3", "C4"]]);
    }

    #[test]
    fn a_duplicated_id_still_has_both_copies_prerequisites_checked() {
        // `derive` refuses a plan with a duplicated id, but the structural checks run before
        // anyone has accepted the plan, so neither copy's prerequisites may go uninspected.
        let plan = plan_of(vec![
            decision("C1", &[]),
            decision("C1", &["C2"]),
            decision("C2", &["C1"]),
            decision("C3", &["C99"]),
        ]);
        assert!(derive(&plan).is_err());
        assert_eq!(prerequisite_cycles(&plan), [["C1", "C2"]]);
        assert_eq!(dangling_prerequisites(&plan), ["C3 -> C99"]);
    }

    #[test]
    fn cycles_do_not_depend_on_the_order_of_the_decisions_vector() {
        let plan = plan_of(vec![
            decision("C1", &["C2"]),
            decision("C2", &["C1"]),
            decision("C4", &["C1"]),
            decision("C10", &["C11"]),
            decision("C11", &["C10"]),
        ]);
        let expected = [["C1", "C2"], ["C10", "C11"]];
        assert_eq!(prerequisite_cycles(&plan), expected);
        assert_eq!(prerequisite_cycles(&plan), expected, "repeated calls agree");

        let mut reversed = plan.clone();
        reversed.decisions.reverse();
        assert_eq!(prerequisite_cycles(&reversed), expected);

        let mut rotated = plan.clone();
        rotated.decisions.rotate_left(3);
        assert_eq!(prerequisite_cycles(&rotated), expected);
    }

    #[test]
    fn a_cycle_on_a_not_applicable_surface_is_still_a_defect() {
        let mut first = decision("C1", &["C2"]);
        first.surface = Surface::S9;
        let mut second = decision("C2", &["C1"]);
        second.surface = Surface::S9;
        let mut plan = plan_of(vec![first, second]);
        mark_not_applicable(&mut plan, Surface::S9);
        assert!(derive(&plan).unwrap().is_empty());
        assert_eq!(prerequisite_cycles(&plan), [["C1", "C2"]]);
    }

    #[test]
    fn a_dangling_prerequisite_is_not_a_cycle() {
        let plan = plan_of(vec![decision("C1", &["C99"])]);
        assert!(prerequisite_cycles(&plan).is_empty());
        assert_eq!(dangling_prerequisites(&plan), ["C1 -> C99"]);
    }

    #[test]
    fn a_plan_whose_prerequisites_all_exist_reports_nothing_dangling() {
        let plan = plan_of(vec![
            decision("C1", &[]),
            decision("C2", &["C1"]),
            decision("C3", &["C1", "C2"]),
        ]);
        assert!(dangling_prerequisites(&plan).is_empty());
    }

    #[test]
    fn dangling_prerequisites_are_deduplicated() {
        let plan = plan_of(vec![
            decision("C7", &["C99", "C99"]),
            decision("C8", &["C99"]),
        ]);
        assert_eq!(dangling_prerequisites(&plan), ["C7 -> C99", "C8 -> C99"]);
    }

    #[test]
    fn dangling_prerequisites_are_ordered_by_number_not_by_text() {
        let plan = plan_of(vec![
            decision("C10", &["C99"]),
            decision("C2", &["C99"]),
            decision("C1", &["C30", "C4"]),
        ]);
        assert_eq!(
            dangling_prerequisites(&plan),
            ["C1 -> C4", "C1 -> C30", "C2 -> C99", "C10 -> C99"]
        );
    }

    #[test]
    fn a_prerequisite_on_a_not_applicable_surface_is_not_dangling() {
        let mut removed = decision("C9", &[]);
        removed.surface = Surface::S9;
        let mut plan = plan_of(vec![removed, decision("C1", &["C9"])]);
        mark_not_applicable(&mut plan, Surface::S9);
        assert!(
            dangling_prerequisites(&plan).is_empty(),
            "the decision still exists; the user only closed its surface"
        );
        assert_eq!(derive(&plan).unwrap().blocked, [blocked("C1", &["C9"])]);
    }

    #[test]
    fn an_empty_prerequisite_id_is_dangling() {
        let plan = plan_of(vec![decision("C1", &[""])]);
        assert_eq!(dangling_prerequisites(&plan), ["C1 -> "]);
        assert_eq!(derive(&plan).unwrap().blocked, [blocked("C1", &[""])]);
    }

    #[test]
    fn a_decision_on_a_surface_the_plan_never_declares_is_corrupt() {
        // An absent surface is not one the user closed: nobody decided anything about it, so
        // hiding C9 here would freeze a plan carrying a question the user was never shown.
        let mut hidden = decision("C9", &[]);
        hidden.surface = Surface::S9;
        let mut plan = plan_of(vec![decision("C1", &[]), hidden]);
        plan.surfaces.remove("S9");
        let err = derive(&plan).unwrap_err();
        assert!(
            matches!(err, Error::Corrupt(_)),
            "unexpected error: {err:?}"
        );
        let message = err.to_string();
        assert!(
            message.contains("never says whether S9 applies"),
            "{message}"
        );
        assert!(message.contains("S9=NA"), "{message}");
        assert!(
            err.is_terminal_for_run(),
            "a corrupt plan cannot be retried into shape"
        );
    }

    #[test]
    fn an_undeclared_surface_that_carries_no_decision_does_not_stop_the_round() {
        let mut plan = plan_of(vec![decision("C1", &[])]);
        plan.surfaces.remove("S9");
        assert_eq!(derive(&plan).unwrap().ready, ["C1"]);
    }

    #[test]
    fn the_undeclared_surface_named_first_does_not_depend_on_decision_order() {
        let mut ninth = decision("C9", &[]);
        ninth.surface = Surface::S9;
        let mut fourth = decision("C4", &[]);
        fourth.surface = Surface::S4;
        let mut plan = plan_of(vec![ninth, fourth]);
        plan.surfaces.remove("S9");
        plan.surfaces.remove("S4");
        let first = derive(&plan).unwrap_err().to_string();
        assert!(first.contains("S4"), "{first}");
        let mut reversed = plan.clone();
        reversed.decisions.reverse();
        assert_eq!(derive(&reversed).unwrap_err().to_string(), first);
    }

    #[test]
    fn a_duplicated_id_is_corrupt_even_when_one_copy_sits_on_a_closed_surface() {
        // The user can re-open S9 next round and the twin comes back with it, so "is C5 answered?"
        // has no single answer now either.
        let mut twin = decision("C5", &[]);
        twin.surface = Surface::S9;
        record_answer(&mut twin, Selection::Recommendation);
        let mut plan = plan_of(vec![decision("C5", &[]), twin]);
        mark_not_applicable(&mut plan, Surface::S9);
        let err = derive(&plan).unwrap_err();
        assert!(
            matches!(err, Error::Corrupt(_)),
            "unexpected error: {err:?}"
        );
        assert!(err.to_string().contains("\"C5\""), "{err}");
    }

    #[test]
    fn both_copies_of_a_duplicated_id_contribute_their_prerequisites() {
        // Keeping only the copy stored last would lose the cycle whenever the edge is on the first.
        let plan = plan_of(vec![
            decision("C1", &["C2"]),
            decision("C1", &[]),
            decision("C2", &["C1"]),
        ]);
        assert_eq!(prerequisite_cycles(&plan), [["C1", "C2"]]);
        let mut swapped = plan.clone();
        swapped.decisions.swap(0, 1);
        assert_eq!(prerequisite_cycles(&swapped), [["C1", "C2"]]);
    }

    #[test]
    fn both_copies_of_a_duplicated_id_report_their_dangling_prerequisites() {
        let plan = plan_of(vec![decision("C1", &["C98"]), decision("C1", &["C99"])]);
        assert_eq!(dangling_prerequisites(&plan), ["C1 -> C98", "C1 -> C99"]);
    }

    #[test]
    fn c0_is_a_number_but_c00_is_not() {
        let ready = ready_of(vec![
            decision("C00", &[]),
            decision("C1", &[]),
            decision("C0", &[]),
        ]);
        assert_eq!(ready, ["C0", "C1", "C00"]);
    }

    #[test]
    fn the_largest_u64_id_is_a_number_and_the_next_one_is_not() {
        let max = format!("C{}", u64::MAX);
        let over = format!("C{}", u128::from(u64::MAX) + 1);
        let ready = ready_of(vec![
            decision(&over, &[]),
            decision(&max, &[]),
            decision("C9", &[]),
        ]);
        assert_eq!(ready, ["C9", max.as_str(), over.as_str()]);
    }

    #[test]
    fn a_multi_byte_id_is_matched_whole_rather_than_by_its_leading_bytes() {
        let plan = plan_of(vec![
            answered("C1", &[]),
            decision("\u{c7}1", &[]),
            decision("C2", &["\u{c7}1"]),
            decision("C3", &["C1"]),
        ]);
        let frontier = derive(&plan).unwrap();
        assert_eq!(frontier.ready, ["C3", "\u{c7}1"]);
        assert_eq!(frontier.blocked, [blocked("C2", &["\u{c7}1"])]);
        assert!(dangling_prerequisites(&plan).is_empty());
    }

    #[test]
    fn cycle_members_that_are_not_c_numbers_sort_by_their_raw_text() {
        let family = "\u{1f469}\u{200d}\u{1f469}\u{200d}\u{1f467}";
        let plan = plan_of(vec![
            decision(family, &["banana"]),
            decision("banana", &[family]),
        ]);
        assert_eq!(prerequisite_cycles(&plan), [["banana", family]]);
    }

    #[test]
    fn the_frontier_survives_a_serialization_round_trip() {
        let mut reworded = answered("C4", &["C2"]);
        reworded.question = "reworded after it was answered".into();
        let mut closed = decision("C5", &[]);
        closed.surface = Surface::S9;
        let mut plan = plan_of(vec![
            answered("C1", &[]),
            decision("C2", &["C1"]),
            decision("C3", &["C99", "C5"]),
            reworded,
            closed,
        ]);
        mark_not_applicable(&mut plan, Surface::S9);
        let decoded: Plan = serde_json::from_str(&serde_json::to_string(&plan).unwrap()).unwrap();
        assert_eq!(derive(&decoded).unwrap(), derive(&plan).unwrap());
        assert_eq!(prerequisite_cycles(&decoded), prerequisite_cycles(&plan));
        assert_eq!(
            dangling_prerequisites(&decoded),
            dangling_prerequisites(&plan)
        );
        assert_eq!(
            derive(&plan).unwrap().blocked,
            [blocked("C3", &["C5", "C99"]), blocked("C4", &["C2"])]
        );
    }

    /// A deterministic generator, so the corpus below is the same on every machine and every run.
    struct Lcg(u64);

    impl Lcg {
        fn below(&mut self, bound: u64) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 33) % bound
        }
    }

    /// Small plans covering every shape the frontier has an opinion about: fresh, stale and missing
    /// answers, closed surfaces, self-edges, cycles and prerequisites that name nothing.
    fn corpus() -> Vec<Plan> {
        let mut rng = Lcg(0x5eed_1234_9abc_def0);
        let mut plans = Vec::new();
        for _ in 0..200 {
            let count = 1 + rng.below(7);
            let mut decisions = Vec::new();
            for n in 1..=count {
                let mut depends_on: Vec<String> = Vec::new();
                for other in 1..=count {
                    if rng.below(3) == 0 {
                        depends_on.push(format!("C{other}"));
                    }
                }
                if rng.below(8) == 0 {
                    depends_on.push("C99".into());
                }
                if rng.below(8) == 0 {
                    depends_on.push(String::new());
                }
                let refs: Vec<&str> = depends_on.iter().map(String::as_str).collect();
                let mut current = decision(&format!("C{n}"), &refs);
                if rng.below(2) == 0 {
                    current.surface = Surface::S9;
                }
                match rng.below(4) {
                    0 => {}
                    1 => record_answer(&mut current, Selection::Recommendation),
                    2 => {
                        record_answer(&mut current, Selection::Recommendation);
                        current.question = format!("C{n} reworded after the answer");
                    }
                    _ => {
                        record_answer(&mut current, Selection::Recommendation);
                        current.recommendation = Recommendation::NoRecommendation {
                            rationale: vec!["re-advised after the answer".into()],
                        };
                    }
                }
                decisions.push(current);
            }
            let mut plan = plan_of(decisions);
            if rng.below(3) == 0 {
                mark_not_applicable(&mut plan, Surface::S9);
            }
            plans.push(plan);
        }
        plans
    }

    /// The cycles a plan has, by mutual reachability rather than by traversal.
    ///
    /// Deliberately the slowest possible definition of "cycle", so it shares no mistake with the
    /// Tarjan pass it is here to check.
    fn cycles_by_reachability(plan: &Plan) -> Vec<Vec<String>> {
        let unique: BTreeSet<&str> = plan.decisions.iter().map(|d| d.id.as_str()).collect();
        let ids: Vec<&str> = unique.into_iter().collect();
        let at = |id: &str| ids.iter().position(|known| *known == id);
        let mut reaches = vec![vec![false; ids.len()]; ids.len()];
        for decision in &plan.decisions {
            for prerequisite in &decision.depends_on {
                if let (Some(from), Some(to)) = (at(&decision.id), at(prerequisite)) {
                    reaches[from][to] = true;
                }
            }
        }
        for via in 0..ids.len() {
            for from in 0..ids.len() {
                for to in 0..ids.len() {
                    if reaches[from][via] && reaches[via][to] {
                        reaches[from][to] = true;
                    }
                }
            }
        }
        let mut grouped = vec![false; ids.len()];
        let mut cycles: Vec<Vec<String>> = Vec::new();
        for from in 0..ids.len() {
            if grouped[from] || !reaches[from][from] {
                continue;
            }
            let mut members: Vec<String> = Vec::new();
            for to in 0..ids.len() {
                if reaches[from][to] && reaches[to][from] {
                    grouped[to] = true;
                    members.push(ids[to].to_string());
                }
            }
            sort_ids(&mut members);
            cycles.push(members);
        }
        cycles.sort_by(|a, b| {
            a.iter()
                .map(|id| order_key(id))
                .cmp(b.iter().map(|id| order_key(id)))
        });
        cycles
    }

    #[test]
    fn tarjan_agrees_with_mutual_reachability_on_every_plan_in_the_corpus() {
        for plan in corpus() {
            assert_eq!(
                prerequisite_cycles(&plan),
                cycles_by_reachability(&plan),
                "disagreed on {:?}",
                plan.decisions
                    .iter()
                    .map(|d| (&d.id, &d.depends_on))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn ready_blocked_and_answered_partition_the_applicable_decisions() {
        for plan in corpus() {
            let frontier = derive(&plan).unwrap();
            let applicable = plan.applicable_surfaces();
            let mut expected: Vec<&str> = plan
                .decisions
                .iter()
                .filter(|d| applicable.contains(&d.surface))
                .map(|d| d.id.as_str())
                .collect();
            expected.sort_unstable();
            let mut seen: Vec<&str> = frontier
                .ready
                .iter()
                .map(String::as_str)
                .chain(frontier.blocked.iter().map(|b| b.id.as_str()))
                .chain(
                    plan.decisions
                        .iter()
                        .filter(|d| applicable.contains(&d.surface) && d.is_answered().unwrap())
                        .map(|d| d.id.as_str()),
                )
                .collect();
            let counted = seen.len();
            seen.sort_unstable();
            seen.dedup();
            assert_eq!(seen.len(), counted, "a decision was counted twice");
            assert_eq!(seen, expected, "a decision was lost or invented");
        }
    }

    #[test]
    fn every_id_the_frontier_waits_on_is_a_real_decision_or_is_reported_dangling() {
        for plan in corpus() {
            let frontier = derive(&plan).unwrap();
            let dangling = dangling_prerequisites(&plan);
            for entry in &frontier.blocked {
                assert!(
                    !entry.waiting_on.is_empty(),
                    "{} waits on nothing",
                    entry.id
                );
                for waited in &entry.waiting_on {
                    if plan.decision(waited).is_none() {
                        let report = format!("{} -> {waited}", entry.id);
                        assert!(
                            dangling.contains(&report),
                            "{report} is not reported dangling"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn every_list_the_frontier_returns_comes_back_in_number_order() {
        for plan in corpus() {
            let frontier = derive(&plan).unwrap();
            let ascending = |ids: &[String]| {
                ids.windows(2)
                    .all(|pair| order_key(&pair[0]) < order_key(&pair[1]))
            };
            assert!(ascending(&frontier.ready), "ready: {:?}", frontier.ready);
            for entry in &frontier.blocked {
                assert!(
                    ascending(&entry.waiting_on),
                    "waiting_on: {:?}",
                    entry.waiting_on
                );
            }
            let blocked_ids: Vec<String> = frontier.blocked.iter().map(|b| b.id.clone()).collect();
            assert!(ascending(&blocked_ids), "blocked: {blocked_ids:?}");
            let stale_ids: Vec<String> = frontier.stale.iter().map(|s| s.id.clone()).collect();
            assert!(ascending(&stale_ids), "stale: {stale_ids:?}");
            for cycle in prerequisite_cycles(&plan) {
                assert!(ascending(&cycle), "cycle: {cycle:?}");
            }
        }
    }

    #[test]
    fn every_stale_decision_is_put_to_the_user_again() {
        for plan in corpus() {
            let frontier = derive(&plan).unwrap();
            for entry in &frontier.stale {
                assert!(
                    matches!(
                        entry.reason,
                        AnswerFreshness::StaleQuestion | AnswerFreshness::StaleRecommendation
                    ),
                    "{entry:?} is not a staleness"
                );
                let asked_again = frontier.ready.contains(&entry.id)
                    || frontier.blocked.iter().any(|b| b.id == entry.id);
                assert!(
                    asked_again,
                    "{} went stale without being asked again",
                    entry.id
                );
            }
        }
    }

    #[test]
    fn the_frontier_does_not_depend_on_the_order_the_decisions_are_stored_in() {
        for plan in corpus() {
            let frontier = derive(&plan).unwrap();
            let cycles = prerequisite_cycles(&plan);
            let dangling = dangling_prerequisites(&plan);
            for shift in 1..plan.decisions.len() {
                let mut shuffled = plan.clone();
                shuffled.decisions.rotate_left(shift);
                shuffled.decisions.reverse();
                assert_eq!(derive(&shuffled).unwrap(), frontier);
                assert_eq!(prerequisite_cycles(&shuffled), cycles);
                assert_eq!(dangling_prerequisites(&shuffled), dangling);
            }
        }
    }
}
