//! Durable state: one atomic snapshot and one append-only hash chain.
//!
//! One repository holds one active run, so there is no scheduling to coordinate and no reason for a
//! database. `run.json` is replaced atomically and `events.jsonl` records every material
//! transition, linked so that a rewritten line is detectable.
//!
//! The journal is the truth and the snapshot is a cache of it. A snapshot *behind* the journal is
//! rebuilt; a snapshot *ahead* of it claims work that was never recorded, which is the one
//! situation Hwahap refuses to reason through — it fails closed instead.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::canonical::{canonical_json, Digest};
use crate::clock::Clock;
use crate::error::{Error, Result};
use crate::plan::{Plan, SCHEMA};

/// The directory Hwahap owns inside the target repository.
pub const DIR: &str = ".hwahap";

/// The journal event kind that carries a full run snapshot.
const SNAPSHOT_KIND: &str = "run_snapshot";
const CONTRACT_SNAPSHOT_KIND: &str = "approved_plan_snapshot";

/// Which of the three cycles a run is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Plan,
    Build,
    Review,
}

impl Phase {
    pub fn name(self) -> &'static str {
        match self {
            Phase::Plan => "plan",
            Phase::Build => "build",
            Phase::Review => "review",
        }
    }
}

/// What the host should do after this call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Next {
    Continue,
    AwaitUser,
    Completed,
    Blocked,
}

/// The engine's resumable position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RunState {
    /// Establishing repository facts.
    Inspecting,
    /// Putting the decision frontier to the user.
    Deciding,
    /// Recompute implications of the user's completed interview round.
    Refining,
    /// Deriving the unit graph and running the two plan reviews.
    Proving,
    /// Waiting for `CONFIRM PLAN <challenge>`.
    AwaitingConfirmation {
        challenge: String,
    },
    /// A confirmed plan retained without starting implementation.
    PlanReady,
    /// Building one unit.
    Coding {
        unit: String,
        attempt: u32,
    },
    /// Full suite and final review.
    FinalVerifying,
    /// A draft exists, but two independent reviews are still required.
    PrReview {
        pr_url: String,
    },
    /// The draft pull request exists; the user adjusts or ships.
    AwaitingAdjustOrShip {
        pr_url: String,
        challenge: String,
    },
    Shipped {
        pr_url: String,
    },
    /// The frozen plan cannot hold; affected decisions must be answered again.
    PlanConflict {
        unit: String,
        detail: String,
    },
    Blocked {
        reason: String,
    },
}

impl RunState {
    /// The stable name reported to the host.
    pub fn name(&self) -> &'static str {
        match self {
            RunState::Inspecting => "inspecting",
            RunState::Deciding => "deciding",
            RunState::Refining => "refining",
            RunState::Proving => "proving",
            RunState::AwaitingConfirmation { .. } => "awaiting_confirmation",
            RunState::PlanReady => "plan_ready",
            RunState::Coding { .. } => "coding",
            RunState::FinalVerifying => "final_verifying",
            RunState::PrReview { .. } => "pr_review",
            RunState::AwaitingAdjustOrShip { .. } => "awaiting_adjust_or_ship",
            RunState::Shipped { .. } => "shipped",
            RunState::PlanConflict { .. } => "plan_conflict",
            RunState::Blocked { .. } => "blocked",
        }
    }

    pub fn phase(&self) -> Phase {
        match self {
            RunState::Inspecting
            | RunState::Deciding
            | RunState::Refining
            | RunState::Proving
            | RunState::AwaitingConfirmation { .. }
            | RunState::PlanReady
            | RunState::PlanConflict { .. } => Phase::Plan,
            RunState::Coding { .. } => Phase::Build,
            RunState::FinalVerifying
            | RunState::PrReview { .. }
            | RunState::AwaitingAdjustOrShip { .. }
            | RunState::Shipped { .. }
            | RunState::Blocked { .. } => Phase::Review,
        }
    }

    pub fn next(&self) -> Next {
        match self {
            // Autonomous states: the host calls straight back without troubling the user.
            RunState::Inspecting
            | RunState::Refining
            | RunState::Proving
            | RunState::Coding { .. }
            | RunState::FinalVerifying
            | RunState::PrReview { .. } => Next::Continue,
            RunState::Deciding
            | RunState::AwaitingConfirmation { .. }
            | RunState::PlanReady
            | RunState::AwaitingAdjustOrShip { .. }
            | RunState::PlanConflict { .. } => Next::AwaitUser,
            RunState::Shipped { .. } => Next::Completed,
            RunState::Blocked { .. } => Next::Blocked,
        }
    }

    /// Whether advancing from here needs a live agent session.
    pub fn needs_sessions(&self) -> bool {
        matches!(
            self,
            RunState::Inspecting
                | RunState::Refining
                | RunState::Proving
                | RunState::Coding { .. }
                | RunState::FinalVerifying
                | RunState::PrReview { .. }
        )
    }

    /// Whether this run is over, so a new request may replace it.
    pub fn is_terminal(&self) -> bool {
        matches!(self, RunState::Shipped { .. } | RunState::Blocked { .. })
    }

    /// The draft pull request, once one exists.
    pub fn pr_url(&self) -> Option<&str> {
        match self {
            RunState::PrReview { pr_url }
            | RunState::AwaitingAdjustOrShip { pr_url, .. }
            | RunState::Shipped { pr_url } => Some(pr_url),
            _ => None,
        }
    }
}

/// The atomic snapshot in `run.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub schema: String,
    pub run_id: String,
    pub goal_id: String,
    pub revision: u32,
    pub state: RunState,
    /// Accepted unit ids, in acceptance order.
    pub accepted_units: Vec<String>,
    /// What each accepted unit was accepted against, by [`crate::plan::Plan::unit_fingerprint`].
    ///
    /// An adjustment rewrites the plan under work that is already committed. This is how the next
    /// freeze tells apart the units the change did not touch, which stay accepted, from the ones it
    /// invalidated, which must be built again.
    pub accepted_fingerprints: std::collections::BTreeMap<String, Digest>,
    /// The frozen plan this run executes.
    #[serde(deserialize_with = "crate::required_option")]
    pub plan_digest: Option<Digest>,
    pub branch: String,
    /// The commit the final review looked at; `SHIP` refuses if the head has moved.
    #[serde(deserialize_with = "crate::required_option")]
    pub reviewed_head: Option<String>,
    /// The journal sequence this snapshot reflects.
    pub seq: u64,
}

impl Run {
    fn require_current(&self) -> Result<()> {
        if self.schema != SCHEMA {
            return Err(Error::Rejected(format!(
                "this .hwahap directory holds a {} run, but Hwahap only supports {SCHEMA}. \
                 Retire the old active state and start a new run; Hwahap does not convert older runs.",
                self.schema
            )));
        }
        for id in &self.accepted_units {
            if !self.accepted_fingerprints.contains_key(id) {
                return Err(corrupt(format!(
                    "accepted unit {id} has no recorded fingerprint"
                )));
            }
        }
        Ok(())
    }
}

/// One journal line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub seq: u64,
    pub ts: String,
    pub prev: Digest,
    pub kind: String,
    pub data: serde_json::Value,
    pub hash: Digest,
}

impl Event {
    /// The hash this event should carry.
    ///
    /// Covers everything except the hash itself, so a rewritten field breaks the link.
    pub fn expected_hash(&self) -> Result<Digest> {
        Digest::of(&serde_json::json!({
            "seq": self.seq,
            "ts": self.ts,
            "prev": self.prev,
            "kind": self.kind,
            "data": self.data,
        }))
    }
}

/// `.hwahap/` on disk.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// Names `<repo_root>/.hwahap` without touching the repository.
    ///
    /// Opening creates nothing: reporting a run's status opens a store, and a read-only tool must
    /// not leave a directory behind in a repository that has never run Hwahap. Every write creates
    /// what it needs on the way.
    pub fn open(repo_root: &Path) -> Result<Store> {
        Ok(Store {
            root: repo_root.join(DIR),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The single run worktree. Git creates it; Hwahap never does.
    pub fn worktree_path(&self) -> PathBuf {
        self.root.join("worktree")
    }

    pub fn artifacts_path(&self) -> PathBuf {
        self.root.join("artifacts")
    }

    /// True when a snapshot or a journal already exists.
    pub fn has_run(&self) -> bool {
        self.run_path().exists() || self.journal_path().exists()
    }

    fn run_path(&self) -> PathBuf {
        self.root.join("run.json")
    }

    fn journal_path(&self) -> PathBuf {
        self.root.join("events.jsonl")
    }

    fn plan_path(&self) -> PathBuf {
        self.root.join("plan.json")
    }

    /// Reads the snapshot, if there is one.
    pub fn read_run(&self) -> Result<Option<Run>> {
        let path = self.run_path();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(Error::io(&path, e)),
        };
        let run: Run = serde_json::from_str(&text)
            .map_err(|e| corrupt(format!("run.json is unreadable: {e}")))?;
        run.require_current()?;
        Ok(Some(run))
    }

    /// Records a run: one journal event, then the snapshot that points at it.
    ///
    /// The clock is a parameter rather than a field so that writing a snapshot without journalling
    /// it is not expressible — that pairing is what makes [`Store::recover`] able to rebuild.
    pub fn write_run(&self, clock: &dyn Clock, run: &Run) -> Result<()> {
        run.require_current()?;
        let mut run = run.clone();
        let snapshot = serde_json::to_value(&run).map_err(|e| Error::Internal(e.to_string()))?;
        let event = self.append_event(clock, SNAPSHOT_KIND, snapshot)?;
        run.seq = event.seq;
        self.write_atomic(&self.run_path(), &to_canonical_line(&run)?)
    }

    /// Reads the plan, if there is one.
    pub fn read_plan(&self) -> Result<Option<Plan>> {
        let path = self.plan_path();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(Error::io(&path, e)),
        };
        // A current-shaped plan with a different schema is rejected too, so the
        // error names what was found instead of "invalid JSON".
        let plan: Plan = serde_json::from_str(&text)
            .map_err(|e| corrupt(format!("plan.json is unreadable: {e}")))?;
        Ok(Some(plan))
    }

    pub fn write_plan(&self, plan: &Plan) -> Result<()> {
        self.write_atomic(&self.plan_path(), &to_canonical_line(plan)?)
    }

    /// Journal the old draft and its approved replacement together before changing either snapshot.
    pub fn write_approved_plan(
        &self,
        clock: &dyn Clock,
        run: &Run,
        plan: &Plan,
        request: &crate::approval::ApprovedPlanRequest,
        pool_scope: &str,
    ) -> Result<()> {
        let event = self.append_event(
            clock,
            CONTRACT_SNAPSHOT_KIND,
            serde_json::json!({
                "run":run, "plan":plan, "request":request, "pool_scope":pool_scope,
                "previous_run":self.read_run()?, "previous_plan":self.read_plan()?
            }),
        )?;
        self.write_plan(plan)?;
        self.write_plan_markdown(&crate::render::plan_markdown(plan)?)?;
        let mut snapshot = run.clone();
        snapshot.seq = event.seq;
        self.write_atomic(&self.run_path(), &to_canonical_line(&snapshot)?)
    }

    pub fn write_plan_markdown(&self, markdown: &str) -> Result<()> {
        self.write_atomic(&self.root.join("plan.md"), markdown)
    }

    pub fn write_report(&self, markdown: &str) -> Result<()> {
        self.write_atomic(&self.root.join("report.md"), markdown)
    }

    pub fn write_usage(&self, json: &str) -> Result<()> {
        self.write_atomic(&self.root.join("usage.json"), json)
    }

    /// Writes `artifacts/<name>`.
    ///
    /// The name comes from an agent's role and a unit id, so it is checked rather than trusted:
    /// nothing may steer a write outside the artifacts directory.
    pub fn write_artifact(&self, name: &str, contents: &str) -> Result<()> {
        if name.is_empty()
            || name.contains('/')
            || name.contains('\\')
            || name.contains("..")
            || name.starts_with('.')
        {
            return Err(Error::Rejected(format!(
                "{name:?} is not a valid artifact name; it must be a plain file name"
            )));
        }
        self.write_atomic(&self.artifacts_path().join(name), contents)
    }

    /// Appends one event, linked to the current tail.
    ///
    /// Reading the tail, repairing it and writing happen under an exclusive lock on the journal,
    /// because `seq` and `prev` are derived from that tail: two writers that read the same tail
    /// would each commit an event with the same sequence number, and no later read can untangle
    /// that.
    pub fn append_event(
        &self,
        clock: &dyn Clock,
        kind: &str,
        data: serde_json::Value,
    ) -> Result<Event> {
        std::fs::create_dir_all(&self.root).map_err(|e| Error::io(&self.root, e))?;
        let path = self.journal_path();
        use std::io::Write as _;
        // `write` rather than `append`: the tail sometimes has to be cut back before the new
        // record goes on, and a handle opened for appending carries no permission to shorten the
        // file — on Windows `set_len` on it fails outright. The write offset is therefore set by
        // hand below rather than implied by the open mode.
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            // Never truncate: the journal is the run's history, and this call is only ever adding
            // one line to the end of it.
            .truncate(false)
            .open(&path)
            .map_err(|e| Error::io(&path, e))?;
        lock_exclusive(&file).map_err(|e| Error::io(&path, e))?;

        let journal = Journal::read(&mut file, &path)?;
        let events = parse_events(&journal.complete)?;
        let (seq, prev) = match events.last() {
            Some(last) => (last.seq + 1, last.hash.clone()),
            None => (1, Digest::zero()),
        };
        let mut event = Event {
            seq,
            ts: clock.now(),
            prev,
            kind: kind.to_string(),
            data,
            hash: Digest::zero(),
        };
        event.hash = event.expected_hash()?;

        // The partial tail [`Store::read_events`] tolerates is cut away before anything is written
        // after it. Appending onto it would glue two records into one line, which parses as
        // nonsense and makes the journal — and with it the run — unreadable for good.
        if journal.partial {
            file.set_len(journal.complete_len)
                .and_then(|()| file.sync_all())
                .map_err(|e| Error::io(&path, e))?;
        }
        use std::io::Seek as _;
        file.seek(std::io::SeekFrom::Start(journal.complete_len))
            .map_err(|e| Error::io(&path, e))?;

        // `to_canonical_line` already terminates the line; adding another newline here would
        // interleave blank lines, and a blank line is exactly what a tamper check skips over.
        let line = to_canonical_line(&event)?;
        file.write_all(line.as_bytes())
            .map_err(|e| Error::io(&path, e))?;
        file.sync_all().map_err(|e| Error::io(&path, e))?;
        Ok(event)
    }

    /// Every complete event, in order.
    ///
    /// A truncated final line is tolerated: that is a crash between `write` and `sync`, and the
    /// event it would have recorded simply did not happen. A malformed line anywhere else means the
    /// file was edited, which is not something to recover from silently.
    pub fn read_events(&self) -> Result<Vec<Event>> {
        let path = self.journal_path();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Error::io(&path, e)),
        };
        parse_events(&Journal::split(&bytes)?.complete)
    }

    /// Verifies sequence continuity and the prev/hash chain.
    pub fn verify_chain(&self) -> Result<()> {
        let events = self.read_events()?;
        let mut previous = Digest::zero();
        for (index, event) in events.iter().enumerate() {
            let expected_seq = index as u64 + 1;
            if event.seq != expected_seq {
                return Err(corrupt(format!(
                    "events.jsonl is out of order: line {} carries sequence {}, expected {expected_seq}",
                    index + 1,
                    event.seq
                )));
            }
            if event.prev != previous {
                return Err(corrupt(format!(
                    "events.jsonl is broken at sequence {}: it links to {} but the previous event \
                     hashes to {previous}",
                    event.seq, event.prev
                )));
            }
            if event.hash != event.expected_hash()? {
                return Err(corrupt(format!(
                    "events.jsonl sequence {} was rewritten: its contents do not hash to the \
                     recorded {}",
                    event.seq, event.hash
                )));
            }
            previous = event.hash.clone();
        }
        Ok(())
    }

    /// Reconciles the snapshot against the journal and returns the run to proceed from.
    pub fn recover(&self) -> Result<Option<Run>> {
        if !self.has_run() {
            return Ok(None);
        }
        self.verify_chain()?;
        let events = self.read_events()?;
        let last_snapshot = events
            .iter()
            .rev()
            .find(|event| event.kind == SNAPSHOT_KIND || event.kind == CONTRACT_SNAPSHOT_KIND);
        let journalled = last_snapshot
            .map(|event| {
                let data = if event.kind == CONTRACT_SNAPSHOT_KIND {
                    event.data["run"].clone()
                } else {
                    event.data.clone()
                };
                serde_json::from_value::<Run>(data).map(|mut run| {
                    run.seq = event.seq;
                    run
                })
            })
            .transpose()
            .map_err(|e| corrupt(format!("a journalled run snapshot is unreadable: {e}")))?;

        if let Some(run) = &journalled {
            run.require_current()?;
        }
        if let Some(event) = last_snapshot.filter(|e| e.kind == CONTRACT_SNAPSHOT_KIND) {
            let plan: Plan = serde_json::from_value(event.data["plan"].clone())
                .map_err(|e| corrupt(format!("approved plan snapshot is unreadable: {e}")))?;
            if self.read_run()?.is_none_or(|run| run.seq < event.seq) {
                // Recover both files even when the crash followed the plan write but preceded run.json.
                self.write_plan(&plan)?;
                self.write_plan_markdown(&crate::render::plan_markdown(&plan)?)?;
            }
        }

        let snapshot = self.read_run()?;
        crate::verification::recover_verifications(self)?;
        match (snapshot, journalled) {
            (None, None) => Ok(None),
            (None, Some(journalled)) => {
                // The snapshot was lost between its journal entry and its atomic rename.
                self.write_atomic(&self.run_path(), &to_canonical_line(&journalled)?)?;
                Ok(Some(journalled))
            }
            (Some(snapshot), None) => Err(corrupt(format!(
                "run.json claims sequence {} but events.jsonl records no run snapshot at all",
                snapshot.seq
            ))),
            (Some(snapshot), Some(journalled)) => {
                if snapshot.seq > journalled.seq {
                    return Err(corrupt(format!(
                        "run.json claims sequence {} but the journal ends at {}; the snapshot \
                         describes work that was never recorded",
                        snapshot.seq, journalled.seq
                    )));
                }
                if snapshot.seq < journalled.seq || snapshot != journalled {
                    self.write_atomic(&self.run_path(), &to_canonical_line(&journalled)?)?;
                    return Ok(Some(journalled));
                }
                Ok(Some(snapshot))
            }
        }
    }

    /// Archived attempts reserve an incarnation even when they stopped before making a branch.
    pub fn archived_run_ids(&self) -> Result<std::collections::BTreeSet<String>> {
        let archive = self.root.join("archive");
        let mut ids = std::collections::BTreeSet::new();
        match std::fs::read_dir(&archive) {
            Ok(entries) => {
                for entry in entries {
                    let path = entry
                        .map_err(|e| Error::io(&archive, e))?
                        .path()
                        .join("run.json");
                    if path.is_file() {
                        let json = std::fs::read(&path).map_err(|e| Error::io(&path, e))?;
                        let run: Run = serde_json::from_slice(&json).map_err(|e| {
                            Error::Rejected(format!(
                                "invalid archived run at {}: {e}",
                                path.display()
                            ))
                        })?;
                        ids.insert(run.run_id);
                    }
                }
                Ok(ids)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ids),
            Err(e) => Err(Error::io(&archive, e)),
        }
    }

    /// Moves the current run's files aside so a new run can start in the same repository.
    ///
    /// `artifacts/` moves with them. Artifact names repeat across runs — every plan's first unit is
    /// `U1`, so its first attempt is always `U1-attempt-1.md` — and leaving the directory in place
    /// would let the next run overwrite the evidence the archived journal still points at.
    pub fn archive(&self, clock: &dyn Clock) -> Result<()> {
        let stamp = clock.now().replace(':', "-");
        let archive = self.root.join("archive");
        std::fs::create_dir_all(&archive).map_err(|e| Error::io(&archive, e))?;
        let mut target = archive.join(&stamp);
        let mut incarnation = 2;
        loop {
            match std::fs::create_dir(&target) {
                Ok(()) => break,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    target = archive.join(format!("{stamp}-{incarnation}"));
                    incarnation += 1;
                }
                Err(e) => return Err(Error::io(&target, e)),
            }
        }
        for name in [
            "run.json",
            "events.jsonl",
            "plan.json",
            "plan.md",
            "report.md",
            "usage.json",
            "verification.json",
            "artifacts",
        ] {
            let from = self.root.join(name);
            if from.exists() {
                std::fs::rename(&from, target.join(name)).map_err(|e| Error::io(&from, e))?;
            }
        }
        Ok(())
    }

    /// Removes the whole directory. Only for a user who abandons a run.
    pub fn destroy(self) -> Result<()> {
        std::fs::remove_dir_all(&self.root).map_err(|e| Error::io(&self.root, e))
    }

    /// Writes through a temp file, fsyncs it, renames it into place, and fsyncs the directory.
    ///
    /// The directory fsync is what actually makes the rename durable; without it a crash can leave
    /// the old file in place even though the new one was synced.
    ///
    /// The temp file is created with `O_EXCL`, which neither follows a symlink nor opens a file
    /// that is already there. The temp name is derivable by anything that can write inside
    /// `.hwahap` — an agent's worktree sits in it — and without `O_EXCL` a symlink planted at that
    /// name would send the write wherever the link points.
    pub(crate) fn write_atomic(&self, path: &Path, contents: &str) -> Result<()> {
        use std::io::Write as _;
        let parent = path.parent().unwrap_or(&self.root);
        std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;

        for attempt in 0..TEMP_ATTEMPTS {
            let temp = temp_path(parent, path, attempt);
            let mut file = match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)
            {
                Ok(file) => file,
                // Another writer's temp file, or one a crash left behind: step to the next name
                // rather than opening — or deleting — a file this write did not create.
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(Error::io(&temp, e)),
            };
            let written = file
                .write_all(contents.as_bytes())
                .and_then(|()| file.sync_all());
            drop(file);
            if let Err(e) = written {
                let _ = std::fs::remove_file(&temp);
                return Err(Error::io(&temp, e));
            }
            if let Err(e) = std::fs::rename(&temp, path) {
                let _ = std::fs::remove_file(&temp);
                return Err(Error::io(path, e));
            }
            // Best effort: some filesystems refuse to open a directory for sync, and failing the
            // write for that would be worse than a slightly weaker durability guarantee.
            if let Ok(dir) = std::fs::File::open(parent) {
                let _ = dir.sync_all();
            }
            return Ok(());
        }
        Err(Error::io(
            parent,
            std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!(
                    "every temporary name for {} is taken; remove the stale .tmp files in {}",
                    path.display(),
                    parent.display()
                ),
            ),
        ))
    }
}

/// How many temp names one write tries before giving up.
///
/// More than one because a name can be taken by a concurrent write or by a crash; few, because a
/// name that is taken four times over is a directory that needs a human.
const TEMP_ATTEMPTS: u32 = 4;

/// The name one write attempt gives its temporary file.
///
/// Scoped to the process and the attempt so that two writers never share a temp file, and so that a
/// temp file left behind by a crash is stepped over instead of reused.
fn temp_path(parent: &Path, path: &Path, attempt: u32) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("hwahap");
    parent.join(format!(".{name}.{}.{attempt}.tmp", std::process::id()))
}

/// A journal file split into the lines that were written whole and the partial tail after them.
struct Journal {
    /// Every byte up to and including the last newline.
    complete: String,
    /// Where the partial tail begins, which is where the next event belongs.
    complete_len: u64,
    /// Whether anything follows that offset.
    partial: bool,
}

impl Journal {
    /// Splits raw journal bytes.
    ///
    /// The split is on bytes rather than characters because a crash can cut a multi-byte character
    /// in half; decoding the whole file first would fail the entire journal over an event that
    /// never happened.
    fn split(bytes: &[u8]) -> Result<Journal> {
        let complete_len = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |index| index + 1);
        let complete = std::str::from_utf8(&bytes[..complete_len])
            .map_err(|e| corrupt(format!("events.jsonl is not valid UTF-8: {e}")))?;
        Ok(Journal {
            complete: complete.to_string(),
            complete_len: complete_len as u64,
            partial: complete_len < bytes.len(),
        })
    }

    /// Reads an open journal from its start.
    fn read(file: &mut std::fs::File, path: &Path) -> Result<Journal> {
        use std::io::Read as _;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|e| Error::io(path, e))?;
        Journal::split(&bytes)
    }
}

/// Parses whole journal lines into events.
///
/// A malformed line means the file was edited, which is not something to recover from silently. A
/// blank one is skipped: it carries nothing, and refusing it would turn a stray newline into a dead
/// repository.
fn parse_events(text: &str) -> Result<Vec<Event>> {
    let mut events = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let event: Event = serde_json::from_str(line).map_err(|e| {
            corrupt(format!(
                "events.jsonl line {} is unreadable: {e}",
                index + 1
            ))
        })?;
        events.push(event);
    }
    Ok(events)
}

/// Corruption, with the one instruction that gets the user moving again.
///
/// Hwahap refuses to rewrite state it cannot read, so a corrupt directory stops every call until a
/// human clears it. The schema mismatch in [`Store::read_run`] already says how; every other report
/// of unreadable state says it too, or the user is told only that they are stuck.
fn corrupt(detail: impl std::fmt::Display) -> Error {
    Error::Corrupt(format!(
        "{detail}. Remove .hwahap to abandon this run and start a new one; Hwahap does not \
         rewrite state it cannot read."
    ))
}

/// Takes an exclusive advisory lock on an open file, waiting for whoever holds it.
///
/// `flock` rather than a lock file: the kernel drops it when the holder exits, so a crash cannot
/// leave a lock nobody can clear. Waiting is bounded because every holder is inside one read and
/// one write of the journal, and the alternative — refusing — would fail a run over a collision
/// that resolves itself in microseconds.
#[cfg(unix)]
fn lock_exclusive(file: &std::fs::File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    loop {
        // SAFETY: `flock` takes a file descriptor and a flag word and touches no memory. The
        // descriptor is owned by `file`, which outlives the call.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        // A signal arriving mid-wait is not a reason to fail a write.
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// Without `flock` there is nothing portable to serialise writers on.
///
/// Hwahap is unix-only for other reasons already — it runs a plan's commands through `sh -c`; see
/// PLATFORM.md §4 — so this is a stub rather than a gap someone relies on.
#[cfg(not(unix))]
fn lock_exclusive(_file: &std::fs::File) -> std::io::Result<()> {
    Ok(())
}

/// Canonical JSON plus a trailing newline, so two writes of the same content are byte-identical.
fn to_canonical_line<T: serde::Serialize>(value: &T) -> Result<String> {
    let json = serde_json::to_value(value).map_err(|e| Error::Internal(e.to_string()))?;
    Ok(format!("{}\n", canonical_json(&json)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        (dir, store)
    }

    fn clock() -> FixedClock {
        FixedClock::new("2026-09-04T00:00:00Z")
    }

    fn a_run() -> Run {
        Run {
            schema: SCHEMA.to_string(),
            run_id: "2026-09-04-dry-run".into(),
            goal_id: "2026-09-04-dry-run".into(),
            revision: 1,
            state: RunState::Deciding,
            accepted_units: Vec::new(),
            accepted_fingerprints: Default::default(),
            plan_digest: None,
            branch: String::new(),
            reviewed_head: None,
            seq: 0,
        }
    }

    #[test]
    fn current_run_requires_every_persisted_field() {
        let run = a_run();
        let saved = serde_json::to_value(&run).unwrap();
        for field in saved.as_object().unwrap().keys() {
            let mut missing = saved.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<Run>(missing).is_err(), "{field}");
        }
        assert_eq!(serde_json::from_value::<Run>(saved).unwrap(), run);
    }

    #[test]
    fn a_fresh_store_has_no_run() {
        let (_dir, store) = store();
        assert!(!store.has_run());
        assert_eq!(store.read_run().unwrap(), None);
        assert_eq!(store.recover().unwrap(), None);
        assert!(store.read_events().unwrap().is_empty());
        store.verify_chain().unwrap();
    }

    #[test]
    fn approved_contract_recovers_both_snapshots_without_losing_the_draft() {
        for write_plan_first in [false, true] {
            let (_dir, store) = store();
            let old = Plan::new("old", "main", "Unfinished interview");
            store.write_plan(&old).unwrap();
            store.write_run(&clock(), &a_run()).unwrap();
            let before = store.read_run().unwrap().unwrap();
            let mut next = before.clone();
            next.revision += 1;
            next.state = RunState::Proving;
            let mut candidate = old.clone();
            candidate.revision = next.revision;
            candidate.goal.statement = "Approved replacement".into();
            let event = store.append_event(&clock(), CONTRACT_SNAPSHOT_KIND,
                serde_json::json!({"run":next,"plan":candidate,"previous_run":before,"previous_plan":old})
            ).unwrap();
            if write_plan_first {
                store.write_plan(&candidate).unwrap();
            }
            let recovered = store.recover().unwrap().unwrap();
            assert_eq!(recovered.seq, event.seq);
            assert_eq!(recovered.revision, 2);
            assert_eq!(store.read_plan().unwrap().unwrap(), candidate);
            assert_eq!(store.recover().unwrap().unwrap(), recovered);
            let events = store.read_events().unwrap();
            assert_eq!(
                events.last().unwrap().data["previous_plan"],
                serde_json::json!(old)
            );
            store.verify_chain().unwrap();
        }
    }

    #[test]
    fn opening_a_store_writes_nothing_into_the_repository() {
        let (dir, store) = store();
        assert_eq!(store.root(), dir.path().join(DIR));
        assert!(
            !dir.path().join(DIR).exists(),
            "`hwahap_status` opens a store and promises to change nothing"
        );
        assert!(!store.has_run());
        assert_eq!(store.read_run().unwrap(), None);
    }

    #[test]
    fn the_first_write_creates_the_artifacts_directory_but_never_the_worktree() {
        let (_dir, store) = store();
        store.write_artifact("F1.md", "content").unwrap();
        assert!(store.artifacts_path().is_dir());
        assert!(!store.worktree_path().exists());
    }

    #[test]
    fn a_written_run_reads_back_exactly() {
        let (_dir, store) = store();
        let run = a_run();
        store.write_run(&clock(), &run).unwrap();
        let read = store.read_run().unwrap().unwrap();
        assert_eq!(read.run_id, run.run_id);
        assert_eq!(read.state, run.state);
        assert_eq!(read.seq, 1, "the snapshot must point at its journal event");
        assert!(store.has_run());
    }

    #[test]
    fn every_run_write_leaves_a_journal_event() {
        let (_dir, store) = store();
        let mut run = a_run();
        store.write_run(&clock(), &run).unwrap();
        run.state = RunState::Proving;
        store.write_run(&clock(), &run).unwrap();

        let events = store.read_events().unwrap();
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|e| e.kind == SNAPSHOT_KIND));
        assert_eq!(events[0].seq, 1);
        assert_eq!(events[1].seq, 2);
        store.verify_chain().unwrap();
    }

    #[test]
    fn the_chain_verifies_over_a_hundred_events() {
        let (_dir, store) = store();
        for n in 0..100 {
            store
                .append_event(&clock(), "test", serde_json::json!({ "n": n }))
                .unwrap();
        }
        store.verify_chain().unwrap();
        let events = store.read_events().unwrap();
        assert_eq!(events.len(), 100);
        assert_eq!(events[0].prev, Digest::zero());
        for pair in events.windows(2) {
            assert_eq!(pair[1].prev, pair[0].hash);
        }
    }

    /// Rewrites one line of the journal through a transformation, for tamper tests.
    fn tamper(store: &Store, line_index: usize, edit: impl Fn(&mut serde_json::Value)) {
        let path = store.journal_path();
        let text = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
        let mut value: serde_json::Value = serde_json::from_str(&lines[line_index]).unwrap();
        edit(&mut value);
        lines[line_index] = serde_json::to_string(&value).unwrap();
        std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
    }

    #[test]
    fn editing_any_field_of_a_middle_event_is_detected() {
        for (label, edit) in [
            (
                "kind",
                Box::new(|v: &mut serde_json::Value| v["kind"] = serde_json::json!("other"))
                    as Box<dyn Fn(&mut serde_json::Value)>,
            ),
            (
                "data",
                Box::new(|v: &mut serde_json::Value| v["data"] = serde_json::json!({"n": 999})),
            ),
            (
                "ts",
                Box::new(|v: &mut serde_json::Value| {
                    v["ts"] = serde_json::json!("2000-01-01T00:00:00Z")
                }),
            ),
            (
                "prev",
                Box::new(|v: &mut serde_json::Value| v["prev"] = serde_json::json!(Digest::zero())),
            ),
            (
                "hash",
                Box::new(|v: &mut serde_json::Value| v["hash"] = serde_json::json!(Digest::zero())),
            ),
        ] {
            let (_dir, store) = store();
            for n in 0..5 {
                store
                    .append_event(&clock(), "test", serde_json::json!({ "n": n }))
                    .unwrap();
            }
            tamper(&store, 2, edit);
            let err = store.verify_chain().unwrap_err();
            assert!(
                matches!(err, Error::Corrupt(_)),
                "editing {label} went undetected: {err:?}"
            );
        }
    }

    #[test]
    fn deleting_a_middle_event_is_detected() {
        let (_dir, store) = store();
        for n in 0..5 {
            store
                .append_event(&clock(), "test", serde_json::json!({ "n": n }))
                .unwrap();
        }
        let path = store.journal_path();
        let text = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<&str> = text.lines().collect();
        lines.remove(2);
        std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();

        let err = store.verify_chain().unwrap_err();
        assert!(err.to_string().contains("out of order"), "{err}");
    }

    #[test]
    fn reordering_two_events_is_detected() {
        let (_dir, store) = store();
        for n in 0..5 {
            store
                .append_event(&clock(), "test", serde_json::json!({ "n": n }))
                .unwrap();
        }
        let path = store.journal_path();
        let text = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<&str> = text.lines().collect();
        lines.swap(1, 2);
        std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
        assert!(store.verify_chain().is_err());
    }

    #[test]
    fn a_truncated_final_line_is_tolerated_and_the_next_append_continues() {
        let (_dir, store) = store();
        for n in 0..3 {
            store
                .append_event(&clock(), "test", serde_json::json!({ "n": n }))
                .unwrap();
        }
        let path = store.journal_path();
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, format!("{text}{{\"seq\":4,\"ts\":\"2026")).unwrap();

        let events = store.read_events().unwrap();
        assert_eq!(
            events.len(),
            3,
            "the partial line must be ignored, not parsed"
        );
        store.verify_chain().unwrap();
    }

    #[test]
    fn a_malformed_line_before_the_tail_is_corruption_not_a_partial_write() {
        let (_dir, store) = store();
        for n in 0..3 {
            store
                .append_event(&clock(), "test", serde_json::json!({ "n": n }))
                .unwrap();
        }
        let path = store.journal_path();
        let text = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
        lines[1] = "{not json".into();
        std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();

        let err = store.read_events().unwrap_err();
        assert!(err.to_string().contains("line 2"), "{err}");
    }

    #[test]
    fn a_snapshot_ahead_of_the_journal_fails_closed() {
        let (_dir, store) = store();
        let mut run = a_run();
        store.write_run(&clock(), &run).unwrap();
        run.seq = 99;
        store
            .write_atomic(&store.run_path(), &to_canonical_line(&run).unwrap())
            .unwrap();

        let err = store.recover().unwrap_err();
        assert!(matches!(err, Error::Corrupt(_)), "{err:?}");
        assert!(err.to_string().contains("never recorded"), "{err}");
    }

    #[test]
    fn a_snapshot_behind_the_journal_is_rebuilt_from_it() {
        let (_dir, store) = store();
        let mut run = a_run();
        store.write_run(&clock(), &run).unwrap();
        let stale = store.read_run().unwrap().unwrap();

        run.state = RunState::Proving;
        store.write_run(&clock(), &run).unwrap();
        // Put the old snapshot back, as if the second rename never landed.
        store
            .write_atomic(&store.run_path(), &to_canonical_line(&stale).unwrap())
            .unwrap();

        let recovered = store.recover().unwrap().unwrap();
        assert_eq!(recovered.state, RunState::Proving);
        assert_eq!(recovered.seq, 2);
        assert_eq!(store.read_run().unwrap().unwrap().state, RunState::Proving);
    }

    #[test]
    fn a_missing_snapshot_is_rebuilt_from_the_journal() {
        let (_dir, store) = store();
        store.write_run(&clock(), &a_run()).unwrap();
        std::fs::remove_file(store.run_path()).unwrap();
        let recovered = store.recover().unwrap().unwrap();
        assert_eq!(recovered.state, RunState::Deciding);
        assert!(store.run_path().exists());
    }

    #[test]
    fn a_snapshot_with_no_journalled_snapshot_at_all_fails_closed() {
        let (_dir, store) = store();
        store
            .append_event(&clock(), "something_else", serde_json::json!({}))
            .unwrap();
        store
            .write_atomic(&store.run_path(), &to_canonical_line(&a_run()).unwrap())
            .unwrap();
        let err = store.recover().unwrap_err();
        assert!(err.to_string().contains("no run snapshot"), "{err}");
    }

    #[test]
    fn an_atomic_write_leaves_no_temporary_file_behind() {
        let (_dir, store) = store();
        store.write_run(&clock(), &a_run()).unwrap();
        store.write_plan_markdown("# plan").unwrap();
        let stray: Vec<String> = std::fs::read_dir(store.root())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(stray.is_empty(), "left behind {stray:?}");
    }

    #[test]
    fn writing_the_same_content_twice_produces_identical_bytes() {
        let (_dir, store) = store();
        let run = a_run();
        store.write_run(&clock(), &run).unwrap();
        let first = std::fs::read(store.run_path()).unwrap();
        store.write_run(&clock(), &run).unwrap();
        let second = std::fs::read(store.run_path()).unwrap();
        // Only `seq` differs between the two writes, and it is what makes them differ; strip it.
        assert_ne!(first, second);
        let mut a: serde_json::Value = serde_json::from_slice(&first).unwrap();
        let mut b: serde_json::Value = serde_json::from_slice(&second).unwrap();
        a["seq"] = serde_json::json!(0);
        b["seq"] = serde_json::json!(0);
        assert_eq!(a, b);
    }

    #[test]
    fn an_artifact_name_may_not_steer_the_write_out_of_the_directory() {
        let (_dir, store) = store();
        for name in [
            "../escape",
            "a/b",
            "a\\b",
            "..",
            "",
            ".hidden",
            "../../etc/passwd",
        ] {
            let err = store.write_artifact(name, "x").unwrap_err();
            assert!(
                err.to_string().contains("valid artifact name"),
                "{name:?} -> {err}"
            );
        }
        store.write_artifact("F1.md", "content").unwrap();
        assert_eq!(
            std::fs::read_to_string(store.artifacts_path().join("F1.md")).unwrap(),
            "content"
        );
    }

    #[test]
    fn obsolete_or_incomplete_journal_cannot_restore_an_active_run() {
        for old_schema in [false, true] {
            let (_dir, store) = store();
            let mut run = a_run();
            if old_schema {
                run.schema = "hwahap/v3".into();
            } else {
                run.accepted_units.push("U1".into());
            }
            assert!(store.write_run(&clock(), &run).is_err());
            store
                .append_event(&clock(), SNAPSHOT_KIND, serde_json::to_value(&run).unwrap())
                .unwrap();
            let journal = std::fs::read(store.journal_path()).unwrap();
            assert!(store.recover().is_err());
            assert!(!store.run_path().exists());
            assert_eq!(std::fs::read(store.journal_path()).unwrap(), journal);
            store
                .write_atomic(&store.run_path(), &to_canonical_line(&run).unwrap())
                .unwrap();
            assert!(store.read_run().is_err());
        }
    }

    #[test]
    fn a_run_json_from_an_older_schema_is_rejected_rather_than_imported() {
        let (_dir, store) = store();
        let mut run = a_run();
        run.schema = "hwahap/v2".into();
        store
            .write_atomic(&store.run_path(), &to_canonical_line(&run).unwrap())
            .unwrap();
        let err = store.read_run().unwrap_err();
        assert!(err.to_string().contains("does not convert"), "{err}");
    }

    #[test]
    fn every_state_reports_a_phase_and_a_next_and_only_two_are_terminal() {
        let states = [
            RunState::Inspecting,
            RunState::Deciding,
            RunState::Proving,
            RunState::AwaitingConfirmation {
                challenge: "ABCD1234".into(),
            },
            RunState::Coding {
                unit: "U1".into(),
                attempt: 1,
            },
            RunState::FinalVerifying,
            RunState::AwaitingAdjustOrShip {
                pr_url: "u".into(),
                challenge: "ABCD1234".into(),
            },
            RunState::Shipped { pr_url: "u".into() },
            RunState::PlanConflict {
                unit: "U1".into(),
                detail: "d".into(),
            },
            RunState::Blocked { reason: "r".into() },
        ];
        let names: std::collections::BTreeSet<_> = states.iter().map(|s| s.name()).collect();
        assert_eq!(names.len(), states.len(), "two states share a name");

        assert_eq!(
            states
                .iter()
                .filter(|s| s.is_terminal())
                .map(|s| s.name())
                .collect::<Vec<_>>(),
            vec!["shipped", "blocked"]
        );
        assert_eq!(
            states
                .iter()
                .filter(|s| s.needs_sessions())
                .map(|s| s.name())
                .collect::<Vec<_>>(),
            vec!["inspecting", "proving", "coding", "final_verifying"]
        );
        assert_eq!(
            states.iter().filter(|s| s.next() == Next::Continue).count(),
            4,
            "exactly the session states continue without the user"
        );
        assert_eq!(states.iter().filter(|s| s.pr_url().is_some()).count(), 2);
    }

    #[test]
    fn every_session_state_continues_and_no_waiting_state_does() {
        for state in [
            RunState::Inspecting,
            RunState::Proving,
            RunState::Coding {
                unit: "U1".into(),
                attempt: 1,
            },
            RunState::FinalVerifying,
        ] {
            assert_eq!(state.next(), Next::Continue, "{}", state.name());
            assert!(state.needs_sessions(), "{}", state.name());
        }
        for state in [
            RunState::Deciding,
            RunState::AwaitingConfirmation {
                challenge: "A".into(),
            },
            RunState::AwaitingAdjustOrShip {
                pr_url: "u".into(),
                challenge: "A".into(),
            },
            RunState::PlanConflict {
                unit: "U1".into(),
                detail: "d".into(),
            },
        ] {
            assert_eq!(state.next(), Next::AwaitUser, "{}", state.name());
            assert!(!state.needs_sessions(), "{}", state.name());
        }
        assert_eq!(
            RunState::Shipped { pr_url: "u".into() }.next(),
            Next::Completed
        );
        assert_eq!(
            RunState::Blocked { reason: "r".into() }.next(),
            Next::Blocked
        );
    }

    #[test]
    fn phases_partition_the_states_as_documented() {
        assert_eq!(RunState::Inspecting.phase(), Phase::Plan);
        assert_eq!(
            RunState::PlanConflict {
                unit: "U1".into(),
                detail: "d".into()
            }
            .phase(),
            Phase::Plan
        );
        assert_eq!(
            RunState::Coding {
                unit: "U1".into(),
                attempt: 1
            }
            .phase(),
            Phase::Build
        );
        assert_eq!(RunState::FinalVerifying.phase(), Phase::Review);
        assert_eq!(Phase::Plan.name(), "plan");
        assert_eq!(Phase::Build.name(), "build");
        assert_eq!(Phase::Review.name(), "review");
    }

    #[test]
    fn a_run_round_trips_through_json_with_every_state_shape() {
        for state in [
            RunState::Inspecting,
            RunState::AwaitingConfirmation {
                challenge: "7F3A91C2".into(),
            },
            RunState::Coding {
                unit: "U3".into(),
                attempt: 2,
            },
            RunState::AwaitingAdjustOrShip {
                pr_url: "https://x/1".into(),
                challenge: "A".into(),
            },
            RunState::PlanConflict {
                unit: "U3".into(),
                detail: "why".into(),
            },
            RunState::Blocked {
                reason: "why".into(),
            },
        ] {
            let mut run = a_run();
            run.state = state.clone();
            run.plan_digest = Some(Digest::of_bytes(b"plan"));
            run.reviewed_head = Some("abc123".into());
            let encoded = serde_json::to_string(&run).unwrap();
            let decoded: Run = serde_json::from_str(&encoded).unwrap();
            assert_eq!(decoded, run, "{state:?} did not round-trip");
        }
    }

    #[test]
    fn archiving_moves_the_run_aside_and_leaves_the_directory_reusable() {
        let (_dir, store) = store();
        store.write_run(&clock(), &a_run()).unwrap();
        store.write_plan_markdown("# plan").unwrap();
        store.archive(&clock()).unwrap();

        assert!(!store.has_run());
        assert_eq!(store.read_run().unwrap(), None);
        assert!(store
            .root()
            .join("archive/2026-09-04T00-00-00Z/run.json")
            .exists());

        store.write_run(&clock(), &a_run()).unwrap();
        assert_eq!(
            store.read_run().unwrap().unwrap().seq,
            1,
            "the new run starts a new chain"
        );
    }

    #[test]
    fn destroy_removes_everything() {
        let (dir, store) = store();
        store.write_run(&clock(), &a_run()).unwrap();
        let root = store.root().to_path_buf();
        store.destroy().unwrap();
        assert!(!root.exists());
        assert!(dir.path().exists());
    }

    #[test]
    fn a_plan_round_trips_through_the_store() {
        let (_dir, store) = store();
        assert_eq!(store.read_plan().unwrap(), None);
        let plan = Plan::new("g", "main", "goal");
        store.write_plan(&plan).unwrap();
        assert_eq!(store.read_plan().unwrap().unwrap(), plan);
    }

    #[test]
    fn a_corrupt_plan_is_reported_as_corruption() {
        let (_dir, store) = store();
        std::fs::create_dir_all(store.root()).unwrap();
        std::fs::write(store.plan_path(), "{not json").unwrap();
        let err = store.read_plan().unwrap_err();
        assert!(matches!(err, Error::Corrupt(_)), "{err:?}");
    }

    #[test]
    fn an_event_hash_covers_every_field_except_itself() {
        let event = Event {
            seq: 1,
            ts: "2026-09-04T00:00:00Z".into(),
            prev: Digest::zero(),
            kind: "test".into(),
            data: serde_json::json!({"a": 1}),
            hash: Digest::zero(),
        };
        let baseline = event.expected_hash().unwrap();

        let mut with_other_hash = event.clone();
        with_other_hash.hash = Digest::of_bytes(b"anything");
        assert_eq!(with_other_hash.expected_hash().unwrap(), baseline);

        for mutate in [
            Box::new(|e: &mut Event| e.seq = 2) as Box<dyn Fn(&mut Event)>,
            Box::new(|e: &mut Event| e.ts = "2026-09-05T00:00:00Z".into()),
            Box::new(|e: &mut Event| e.prev = Digest::of_bytes(b"x")),
            Box::new(|e: &mut Event| e.kind = "other".into()),
            Box::new(|e: &mut Event| e.data = serde_json::json!({"a": 2})),
        ] {
            let mut mutated = event.clone();
            mutate(&mut mutated);
            assert_ne!(mutated.expected_hash().unwrap(), baseline);
        }
    }

    #[test]
    fn an_empty_journal_line_is_skipped_rather_than_failing() {
        let (_dir, store) = store();
        store
            .append_event(&clock(), "test", serde_json::json!({}))
            .unwrap();
        let path = store.journal_path();
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, format!("\n{text}\n")).unwrap();
        assert_eq!(store.read_events().unwrap().len(), 1);
    }

    #[test]
    fn artifacts_with_unicode_names_and_contents_survive() {
        let (_dir, store) = store();
        store.write_artifact("결정-C1.md", "내용 ✅").unwrap();
        assert_eq!(
            std::fs::read_to_string(store.artifacts_path().join("결정-C1.md")).unwrap(),
            "내용 ✅"
        );
    }

    #[test]
    fn p1_appending_after_a_partial_tail_destroys_the_journal() {
        let (_dir, store) = store();
        for n in 0..3 {
            store
                .append_event(&clock(), "test", serde_json::json!({ "n": n }))
                .unwrap();
        }
        let path = store.journal_path();
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, format!("{text}{{\"seq\":4,\"ts\":\"2026")).unwrap();

        store
            .append_event(&clock(), "test", serde_json::json!({ "n": 3 }))
            .unwrap();
        let events = store.read_events().unwrap();
        assert_eq!(events.len(), 4);
        store.verify_chain().unwrap();
    }

    #[test]
    fn p1b_a_tail_cut_through_a_multi_byte_character_is_repaired() {
        let (_dir, store) = store();
        store
            .append_event(&clock(), "test", serde_json::json!({ "n": 0 }))
            .unwrap();
        let path = store.journal_path();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.extend_from_slice(b"{\"data\":\"\xea\xb2");
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(store.read_events().unwrap().len(), 1);
        store
            .append_event(&clock(), "test", serde_json::json!({ "n": 1 }))
            .unwrap();
        store.verify_chain().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn p3_a_planted_temp_symlink_steers_the_write_out_of_the_directory() {
        let (dir, store) = store();
        let outside = dir.path().join("outside.txt");
        std::fs::write(&outside, "original\n").unwrap();
        std::fs::create_dir_all(store.artifacts_path()).unwrap();
        std::os::unix::fs::symlink(&outside, store.artifacts_path().join(".F1.md.tmp")).unwrap();
        store.write_artifact("F1.md", "steered").unwrap();
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "original\n");
    }

    #[cfg(unix)]
    #[test]
    fn p3b_a_planted_temp_symlink_turns_run_json_into_a_symlink() {
        let (dir, store) = store();
        let outside = dir.path().join("outside.json");
        std::fs::write(&outside, "{}\n").unwrap();
        std::fs::create_dir_all(store.root()).unwrap();
        std::os::unix::fs::symlink(&outside, store.root().join(".run.json.tmp")).unwrap();
        store.write_run(&clock(), &a_run()).unwrap();
        let meta = std::fs::symlink_metadata(store.run_path()).unwrap();
        assert!(!meta.file_type().is_symlink());
    }

    // Unix only: the writer lock is `flock`, and there is no portable equivalent. Hwahap is
    // unix-only for other reasons already (PLATFORM.md §4), so this documents the guarantee where
    // it exists rather than pretending it exists everywhere.
    #[cfg(unix)]
    #[test]
    fn p7_two_writers_collide_and_brick_the_journal() {
        let (_dir, store) = store();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let mut handles = Vec::new();
        for _ in 0..4 {
            let store = store.clone();
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..40 {
                    let _ = store.write_run(&FixedClock::new("2026-09-04T00:00:00Z"), &a_run());
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        store.verify_chain().unwrap();
    }

    #[test]
    fn p8_a_new_run_overwrites_the_archived_runs_evidence() {
        let (_dir, store) = store();
        store.write_run(&clock(), &a_run()).unwrap();
        store
            .write_artifact("U1-attempt-1.md", "the first run")
            .unwrap();
        store.archive(&clock()).unwrap();
        store.write_run(&clock(), &a_run()).unwrap();
        store
            .write_artifact("U1-attempt-1.md", "the second run")
            .unwrap();
        assert!(store
            .root()
            .join("archive/2026-09-04T00-00-00Z/artifacts/U1-attempt-1.md")
            .exists());
    }

    #[test]
    fn p9_merely_opening_the_store_creates_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let _store = Store::open(dir.path()).unwrap();
        assert!(!dir.path().join(DIR).exists());
    }
}
