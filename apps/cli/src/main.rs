use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use liferuntime_agent_bridge::{AgentBridge, FakeAgent, ProposedEvent, SignalAnalysisInput};
use liferuntime_event_log::EventId;
use liferuntime_world::{
    format_decision_list, ChangeRecord, Explanation, ProjectStatus, ProjectTrajectoryView,
    ProjectView, WorldChanges, WorldEvent, WorldRuntime,
};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "liferuntime",
    about = "Persistent world simulation runtime",
    version
)]
struct Cli {
    /// Directory where events.jsonl, cursor.json, advances.jsonl,
    /// last_advance.json live.
    #[arg(long, global = true, default_value = ".liferuntime")]
    dir: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a fresh runtime directory.
    Init,

    /// Project commands.
    #[command(subcommand)]
    Project(ProjectCmd),

    /// Goal commands.
    #[command(subcommand)]
    Goal(GoalCmd),

    /// Signal commands.
    #[command(subcommand)]
    Signal(SignalCmd),

    /// Run systems, derive changes since the last advance, persist them.
    Advance,

    /// Inspect entity state.
    #[command(subcommand)]
    Inspect(InspectCmd),

    /// Render the last persisted advance as a human-readable explanation.
    #[command(subcommand)]
    Explain(ExplainCmd),

    /// Print the number of stored events (proof of replay).
    Replay,

    /// Surface what's pulling on attention: active projects ranked by
    /// current strategic_relevance with their recent Trajectory
    /// (↑ warming / ↓ cooling / → stable).
    Status {
        /// Number of recent Advances to compute Trajectory slope over.
        #[arg(long, default_value_t = 5)]
        window: usize,
    },

    /// Advance event-log time without an underlying world event. Used
    /// during quiet stretches (vacation, low activity) so Decay can catch
    /// up. Replay-safe — the pulse is itself a logged Event.
    Pulse {
        /// Override the pulse timestamp. Defaults to wall-clock now.
        #[arg(long)]
        at: Option<DateTime<Utc>>,
        /// Optional idempotency key. Useful for cron retries.
        #[arg(long = "idempotency-key")]
        idempotency_key: Option<String>,
    },

    /// Record a strategic Decision (ADR-0008): commit to one Project,
    /// optionally over named rivals, optionally dampening explicit
    /// suppression targets. This slice (issue #1) plumbs the event
    /// end-to-end; the boost / dampening mechanics arrive in later
    /// slices.
    Decide {
        /// Committed-to Project id.
        #[arg(long)]
        chose: String,
        /// Narrative rival Project ids; **no mechanical effect** (per
        /// ADR-0008). Surfaced later by `decision list` / `explain`.
        #[arg(long, value_delimiter = ',')]
        over: Vec<String>,
        /// Project ids whose resonance deltas should be mechanically
        /// dampened. Empty by default — the user must opt in.
        #[arg(long, value_delimiter = ',')]
        dampen: Vec<String>,
        /// Free-text rationale for the decision.
        #[arg(long = "because")]
        reason: Option<String>,
        /// Override the decision timestamp. Defaults to ingest time
        /// (i.e. the envelope's `occurred_at`).
        #[arg(long = "decided-at")]
        decided_at: Option<DateTime<Utc>>,
    },

    /// Decision queries (per-project stance, future polish in issue #7).
    #[command(subcommand)]
    Decision(DecisionCmd),
}

#[derive(Subcommand)]
enum DecisionCmd {
    /// Print active per-project Decision stances.
    ///
    /// Minimal format in this slice (issue #2). The full block format
    /// with boost remaining, decided-on date, and partial supersession
    /// lands in issue #7.
    List,
    /// Revoke a previously-recorded Decision by its event id (ADR-0008
    /// `#lifecycle`). Every Project whose stance is still owned by the
    /// revoked Decision reverts to no-stance.
    Revoke {
        /// Event id of the `DecisionRecorded` to revoke.
        decision_id: String,
    },
}

#[derive(Subcommand)]
enum ProjectCmd {
    Add {
        id: String,
        #[arg(long)]
        name: String,
        #[arg(long, value_delimiter = ',')]
        tags: Vec<String>,
    },
    /// Edit an existing project. Only provided fields are updated.
    /// `--tags` replaces the project's tag list (use the full new list).
    /// `--depends-on` replaces the project's dependency annotation list
    /// (CONTEXT.md `#depends_on`); same full-replace ergonomics as
    /// `--tags`.
    Edit {
        id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, value_delimiter = ',')]
        tags: Option<Vec<String>>,
        #[arg(long = "depends-on", value_delimiter = ',')]
        depends_on: Option<Vec<String>>,
    },
    /// Soft-shelve a project. Systems skip it; reactivate to un-do.
    Archive {
        id: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Mark a project done. Systems skip it; reactivate to un-do.
    Complete {
        id: String,
        #[arg(long)]
        note: Option<String>,
    },
    /// Move an Archived or Completed project back to Active.
    Reactivate { id: String },
}

#[derive(Subcommand)]
enum GoalCmd {
    Add {
        id: String,
        #[arg(long)]
        name: String,
        #[arg(long, value_delimiter = ',')]
        tags: Vec<String>,
        #[arg(long, default_value_t = 0.5)]
        importance: f32,
    },
    /// Edit an existing goal. Only provided fields are updated.
    Edit {
        id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, value_delimiter = ',')]
        tags: Option<Vec<String>>,
        #[arg(long)]
        importance: Option<f32>,
    },
    /// Mark a goal as reached. Stops amplifying matching.
    Achieve {
        id: String,
        #[arg(long)]
        note: Option<String>,
    },
    /// Mark a goal as given up. Stops amplifying matching.
    Abandon {
        id: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Move an Achieved or Abandoned goal back to Active.
    Reactivate { id: String },
}

#[derive(Subcommand)]
enum SignalCmd {
    /// Record a signal directly.
    Add {
        #[arg(long)]
        source: String,
        #[arg(long)]
        summary: String,
        #[arg(long, value_delimiter = ',')]
        tags: Vec<String>,
        #[arg(long, default_value_t = 0.5)]
        confidence: f32,
        /// Optional idempotency key. A second `signal add` with the
        /// same key is a no-op — useful for cron retries.
        #[arg(long = "idempotency-key")]
        idempotency_key: Option<String>,
    },
    /// Run an `AgentBridge` adapter (FakeAgent in v1) on free text to
    /// generate proposed signals. The runtime does not ingest them
    /// automatically; pass `--commit` to ingest each proposal.
    Analyze {
        text: String,
        #[arg(long, value_delimiter = ',')]
        tags: Vec<String>,
        /// Ingest the proposed events as SignalObserved after printing them.
        #[arg(long)]
        commit: bool,
        /// Optional idempotency-key *prefix*. Each proposed event gets
        /// `{prefix}#{n}` as its key, so a second run with the same
        /// prefix dedupes against the first.
        #[arg(long = "idempotency-key")]
        idempotency_key: Option<String>,
    },
}

#[derive(Subcommand)]
enum InspectCmd {
    Project { id: String },
}

#[derive(Subcommand)]
enum ExplainCmd {
    /// Render the most recent advance — the "what just happened" view.
    Latest,
    /// Show changes for a Project. Default is the most-recent-Advance
    /// window (mirrors `explain latest`, filtered by entity_id).
    /// `--all` (alias `--full`) prints every ChangeRecord that has ever
    /// touched the project, in event-log order — the "git log" view.
    Project {
        id: String,
        /// Print the full causal history (CONTEXT.md `#explanation`).
        #[arg(long, alias = "full")]
        all: bool,
    },
}

fn main() {
    if let Err(err) = run() {
        eprint_friendly(&err);
        std::process::exit(1);
    }
}

fn eprint_friendly(err: &anyhow::Error) {
    // Print the top-level message clearly, then walk the cause chain.
    eprintln!("error: {err}");
    let mut source = err.source();
    while let Some(cause) = source {
        eprintln!("  cause: {cause}");
        source = cause.source();
    }

    // Pattern-match the error string for actionable hints. We can't
    // downcast cleanly through anyhow without exporting the concrete
    // error types from every layer, and string-matching covers the
    // common cases without leaking types.
    let msg = format!("{err}");
    if msg.contains("already exists") {
        eprintln!(
            "  hint: use the `edit` subcommand to modify the existing entity, \
                   or pick a different id."
        );
    } else if msg.contains("not found") {
        eprintln!("  hint: list entities with `liferuntime status` or `liferuntime inspect`.");
    } else if msg.contains("out of range") {
        eprintln!("  hint: confidence and importance must be in [0.0, 1.0].");
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Init => cmd_init(&cli.dir)?,
        Command::Project(ProjectCmd::Add { id, name, tags }) => {
            let mut rt = WorldRuntime::open_dir(&cli.dir)?;
            rt.ingest(WorldEvent::ProjectCreated {
                id: id.clone(),
                name,
                tags,
            })?;
            println!("Project added: {id}");
        }
        Command::Project(ProjectCmd::Edit {
            id,
            name,
            tags,
            depends_on,
        }) => {
            let mut rt = WorldRuntime::open_dir(&cli.dir)?;
            rt.ingest(WorldEvent::ProjectUpdated {
                id: id.clone(),
                name,
                tags,
                depends_on,
            })?;
            println!("Project updated: {id}");
        }
        Command::Project(ProjectCmd::Archive { id, reason }) => {
            let mut rt = WorldRuntime::open_dir(&cli.dir)?;
            rt.ingest(WorldEvent::ProjectArchived {
                id: id.clone(),
                reason,
            })?;
            println!("Project archived: {id}");
        }
        Command::Project(ProjectCmd::Complete { id, note }) => {
            let mut rt = WorldRuntime::open_dir(&cli.dir)?;
            rt.ingest(WorldEvent::ProjectCompleted {
                id: id.clone(),
                note,
            })?;
            println!("Project completed: {id}");
        }
        Command::Project(ProjectCmd::Reactivate { id }) => {
            let mut rt = WorldRuntime::open_dir(&cli.dir)?;
            rt.ingest(WorldEvent::ProjectReactivated { id: id.clone() })?;
            println!("Project reactivated: {id}");
        }
        Command::Goal(GoalCmd::Add {
            id,
            name,
            tags,
            importance,
        }) => {
            let mut rt = WorldRuntime::open_dir(&cli.dir)?;
            rt.ingest(WorldEvent::GoalCreated {
                id: id.clone(),
                name,
                tags,
                importance,
            })?;
            println!("Goal added: {id}");
        }
        Command::Goal(GoalCmd::Edit {
            id,
            name,
            tags,
            importance,
        }) => {
            let mut rt = WorldRuntime::open_dir(&cli.dir)?;
            rt.ingest(WorldEvent::GoalUpdated {
                id: id.clone(),
                name,
                tags,
                importance,
            })?;
            println!("Goal updated: {id}");
        }
        Command::Goal(GoalCmd::Achieve { id, note }) => {
            let mut rt = WorldRuntime::open_dir(&cli.dir)?;
            rt.ingest(WorldEvent::GoalAchieved {
                id: id.clone(),
                note,
            })?;
            println!("Goal achieved: {id}");
        }
        Command::Goal(GoalCmd::Abandon { id, reason }) => {
            let mut rt = WorldRuntime::open_dir(&cli.dir)?;
            rt.ingest(WorldEvent::GoalAbandoned {
                id: id.clone(),
                reason,
            })?;
            println!("Goal abandoned: {id}");
        }
        Command::Goal(GoalCmd::Reactivate { id }) => {
            let mut rt = WorldRuntime::open_dir(&cli.dir)?;
            rt.ingest(WorldEvent::GoalReactivated { id: id.clone() })?;
            println!("Goal reactivated: {id}");
        }
        Command::Signal(SignalCmd::Add {
            source,
            summary,
            tags,
            confidence,
            idempotency_key,
        }) => {
            let mut rt = WorldRuntime::open_dir(&cli.dir)?;
            rt.ingest_with_key(
                WorldEvent::SignalObserved {
                    source,
                    summary: summary.clone(),
                    tags,
                    confidence,
                    observed_at: None,
                },
                idempotency_key,
            )?;
            println!("Signal recorded: {summary}");
        }
        Command::Signal(SignalCmd::Analyze {
            text,
            tags,
            commit,
            idempotency_key,
        }) => {
            let proposals = FakeAgent.analyze_signal(SignalAnalysisInput { text, hints: tags })?;
            if proposals.is_empty() {
                println!("Agent proposed no events.");
            } else {
                print_proposals(&proposals);
                if commit {
                    let mut rt = WorldRuntime::open_dir(&cli.dir)?;
                    for (n, p) in proposals.iter().enumerate() {
                        let key = idempotency_key
                            .as_ref()
                            .map(|prefix| format!("{prefix}#{}", n + 1));
                        rt.ingest_with_key(
                            WorldEvent::SignalObserved {
                                source: p.source.clone(),
                                summary: p.summary.clone(),
                                tags: p.tags.clone(),
                                confidence: p.confidence,
                                observed_at: None,
                            },
                            key,
                        )?;
                    }
                    println!("Committed {} signal(s) to the log.", proposals.len());
                } else {
                    println!("(dry run — re-run with --commit to ingest)");
                }
            }
        }
        Command::Advance => {
            let mut rt = WorldRuntime::open_dir(&cli.dir)?;
            let changes = rt.advance()?;
            print_advance(&cli.dir, &changes)?;
        }
        Command::Inspect(InspectCmd::Project { id }) => {
            // Under ADR-0006 (per-event scheduling), open_dir already
            // derives state. No materialize step needed.
            let mut rt = WorldRuntime::open_dir(&cli.dir)?;
            match rt.inspect_project(&id) {
                Some(view) => print_project(&view),
                None => println!("Project not found: {id}"),
            }
        }
        Command::Explain(ExplainCmd::Latest) => {
            let rt = WorldRuntime::open_dir(&cli.dir)?;
            let pending = rt.pending_events()?;
            drop(rt); // release flock before printing

            match load_last_advance(&cli.dir)? {
                Some(e) => {
                    if pending > 0 {
                        println!(
                            "(stale — {pending} event(s) ingested since the last advance; \
                             run `liferuntime advance` to refresh)",
                        );
                        println!();
                    }
                    print!("{e}");
                }
                None => {
                    println!("No advance has been recorded yet. Run `liferuntime advance`.");
                }
            }
        }
        Command::Explain(ExplainCmd::Project { id, all }) => {
            let mut rt = WorldRuntime::open_dir(&cli.dir)?;
            if all {
                // Full causal history. `explain_project_history` does
                // the entity-existence check; an unknown id propagates
                // as `EntityNotFound` and the CLI exits non-zero via
                // `eprint_friendly`.
                let explanation = rt.explain_project_history(&id)?;
                if explanation.records.is_empty() {
                    let view = rt
                        .inspect_project(&id)
                        .expect("project_exists already verified by explain_project_history");
                    print_empty_history_state("Project", &view);
                } else {
                    print!("{explanation}");
                }
            } else {
                // Most-recent-Advance window filtered by entity. Probe
                // existence first so unknown ids exit non-zero.
                let view = match rt.inspect_project(&id) {
                    Some(v) => v,
                    None => anyhow::bail!("Project not found: {id}"),
                };
                let pending = rt.pending_events()?;
                drop(rt);
                match load_last_advance(&cli.dir)? {
                    Some(e) => {
                        let filtered: Vec<ChangeRecord> = e
                            .records
                            .into_iter()
                            .filter(|r| r.entity_id == id)
                            .collect();
                        if pending > 0 {
                            println!(
                                "(stale — {pending} event(s) ingested since the last advance; \
                                 run `liferuntime advance` to refresh)",
                            );
                            println!();
                        }
                        if filtered.is_empty() {
                            println!(
                                "No changes for project {id} in the most recent advance. \
                                 Pass --all to see the full history.",
                            );
                            println!();
                            println!("Current state (from inspect):");
                            println!("{}", one_line_project_summary(&view));
                        } else {
                            let explanation = Explanation { records: filtered };
                            print!("{explanation}");
                        }
                    }
                    None => {
                        println!("No advance has been recorded yet. Run `liferuntime advance`.");
                    }
                }
            }
        }
        Command::Replay => {
            let rt = WorldRuntime::open_dir(&cli.dir)?;
            println!(
                "Replayed {} events from {}.",
                rt.event_count(),
                cli.dir.display()
            );
        }
        Command::Status { window } => {
            let mut rt = WorldRuntime::open_dir(&cli.dir)?;
            let trajectories = rt.trajectories(window)?;
            print_status(&trajectories, window);
        }
        Command::Pulse {
            at,
            idempotency_key,
        } => {
            let mut rt = WorldRuntime::open_dir(&cli.dir)?;
            let observed_at = at.unwrap_or_else(Utc::now);
            rt.ingest_with_key(
                WorldEvent::TimePulseObserved { observed_at },
                idempotency_key,
            )?;
            println!("Pulse recorded at {observed_at}");
        }
        Command::Decide {
            chose,
            over,
            dampen,
            reason,
            decided_at,
        } => {
            let mut rt = WorldRuntime::open_dir(&cli.dir)?;
            rt.ingest(WorldEvent::DecisionRecorded {
                chose: chose.clone(),
                over,
                dampen,
                reason,
                decided_at,
            })?;
            println!("Decision recorded: chose {chose}");
        }
        Command::Decision(DecisionCmd::List) => {
            let mut rt = WorldRuntime::open_dir(&cli.dir)?;
            let views = rt.decision_list()?;
            if views.is_empty() {
                println!("No active decisions.");
            } else {
                // `format_decision_list` always ends with a newline so
                // we use `print!` rather than `println!` to avoid an
                // extra blank line at the end.
                print!("{}", format_decision_list(&views));
            }
        }
        Command::Decision(DecisionCmd::Revoke { decision_id }) => {
            let mut rt = WorldRuntime::open_dir(&cli.dir)?;
            rt.ingest(WorldEvent::DecisionRevoked {
                decision_id: EventId(decision_id.clone()),
            })?;
            println!("Decision revoked: {decision_id}");
        }
    }
    Ok(())
}

fn cmd_init(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let events = dir.join("events.jsonl");
    if !events.exists() {
        std::fs::File::create(&events).with_context(|| format!("creating {}", events.display()))?;
    }
    println!("Initialized liferuntime at {}", dir.display());
    Ok(())
}

fn print_advance(dir: &Path, changes: &WorldChanges) -> Result<()> {
    if changes.is_empty() {
        println!("No new changes since last advance.");
        return Ok(());
    }
    let explanation = Explanation {
        records: changes.records.clone(),
    };
    print!("{explanation}");
    save_last_advance(dir, &explanation)?;
    Ok(())
}

/// One-line condensed project summary for the `explain --all` empty
/// state and the default-window "no changes for X" branch (issue #16).
/// Mirrors the fields the multi-line `inspect` view prints, compressed
/// onto a single line so the empty-state block stays scannable.
fn one_line_project_summary(p: &ProjectView) -> String {
    let status_label = match p.status {
        ProjectStatus::Active => "active",
        ProjectStatus::Archived => "archived",
        ProjectStatus::Completed => "completed",
    };
    let depends_part = if p.depends_on.is_empty() {
        String::new()
    } else {
        format!(" depends_on=[{}]", p.depends_on.join(", "))
    };
    format!(
        "Project: {} ({}) [{}] tags=[{}]{} relevance={:.2} urgency={:.2}",
        p.name,
        p.id,
        status_label,
        p.tags.join(", "),
        depends_part,
        p.strategic_relevance_visible,
        p.urgency,
    )
}

/// Friendly "no changes yet" block for the `--all` empty state (issue
/// #16). Symmetric with `git log` printing an empty-state hint instead
/// of returning silently.
fn print_empty_history_state(kind: &str, p: &ProjectView) {
    println!("No changes yet for {kind} {}.", p.id);
    println!();
    println!("Current state (from inspect):");
    println!("{}", one_line_project_summary(p));
}

fn print_project(p: &ProjectView) {
    let status_label = match p.status {
        ProjectStatus::Active => "active".to_string(),
        ProjectStatus::Archived => match &p.archived_reason {
            Some(r) => format!("archived ({r})"),
            None => "archived".into(),
        },
        ProjectStatus::Completed => match &p.completion_note {
            Some(n) => format!("completed ({n})"),
            None => "completed".into(),
        },
    };
    println!("Project: {} ({}) [{}]", p.name, p.id, status_label);
    println!("Tags: [{}]", p.tags.join(", "));
    // Per CONTEXT.md `#depends_on`: declarative annotation surfaced in
    // `inspect` only. Empty list ⇒ omit the line.
    if !p.depends_on.is_empty() {
        println!("Depends on: [{}]", p.depends_on.join(", "));
    }
    println!(
        "  strategic_relevance: raw {:.2}  visible {:.2}",
        p.strategic_relevance_raw, p.strategic_relevance_visible,
    );
    println!("  urgency:             {:.2}", p.urgency);
}

fn print_status(views: &[ProjectTrajectoryView], window: usize) {
    let mut active: Vec<&ProjectTrajectoryView> = views
        .iter()
        .filter(|v| v.status == ProjectStatus::Active)
        .collect();
    active.sort_by(|a, b| {
        b.current_relevance
            .partial_cmp(&a.current_relevance)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    if active.is_empty() {
        println!("No active projects. Add one with `liferuntime project add`.");
        return;
    }

    println!(
        "Active projects ({}, trajectory over last {} advance(s)):",
        active.len(),
        window
    );
    for v in &active {
        let arrow = if v.advances_observed == 0 {
            "·"
        } else if v.slope_relevance > 0.02 {
            "↑"
        } else if v.slope_relevance < -0.02 {
            "↓"
        } else {
            "→"
        };
        let label = match arrow {
            "↑" => "warming",
            "↓" => "cooling",
            "→" => "stable",
            _ => "quiet",
        };
        println!(
            "  {} {:24}  relevance {:.2}  urgency {:.2}  slope {:+.3} ({})",
            arrow, v.name, v.current_relevance, v.current_urgency, v.slope_relevance, label,
        );
    }
}

fn print_proposals(proposals: &[ProposedEvent]) {
    println!("Proposed by agent:");
    for (i, p) in proposals.iter().enumerate() {
        println!(
            "  [{}] {} (confidence {:.2})",
            i + 1,
            p.summary,
            p.confidence
        );
        if !p.tags.is_empty() {
            println!("       tags: [{}]", p.tags.join(", "));
        }
        if !p.rationale.is_empty() {
            println!("       rationale: {}", p.rationale);
        }
    }
}

fn save_last_advance(dir: &Path, e: &Explanation) -> Result<()> {
    let path = dir.join("last_advance.json");
    let bytes = serde_json::to_vec_pretty(e)?;
    std::fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn load_last_advance(dir: &Path) -> Result<Option<Explanation>> {
    let path = dir.join("last_advance.json");
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    Ok(Some(serde_json::from_slice(&bytes)?))
}
