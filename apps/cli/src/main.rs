use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use liferuntime_agent_bridge::{AgentBridge, FakeAgent, ProposedEvent, SignalAnalysisInput};
use liferuntime_world::{
    Explanation, ProjectStatus, ProjectView, WorldChanges, WorldEvent, WorldRuntime,
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

    /// Advance event-log time without an underlying world event. Used
    /// during quiet stretches (vacation, low activity) so Decay can catch
    /// up. Replay-safe — the pulse is itself a logged Event.
    Pulse {
        /// Override the pulse timestamp. Defaults to wall-clock now.
        #[arg(long)]
        at: Option<DateTime<Utc>>,
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
    Edit {
        id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, value_delimiter = ',')]
        tags: Option<Vec<String>>,
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
    Reactivate {
        id: String,
    },
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
    },
}

#[derive(Subcommand)]
enum InspectCmd {
    Project { id: String },
}

#[derive(Subcommand)]
enum ExplainCmd {
    Latest,
}

fn main() -> Result<()> {
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
        Command::Project(ProjectCmd::Edit { id, name, tags }) => {
            let mut rt = WorldRuntime::open_dir(&cli.dir)?;
            rt.ingest(WorldEvent::ProjectUpdated {
                id: id.clone(),
                name,
                tags,
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
        Command::Signal(SignalCmd::Add {
            source,
            summary,
            tags,
            confidence,
        }) => {
            let mut rt = WorldRuntime::open_dir(&cli.dir)?;
            rt.ingest(WorldEvent::SignalObserved {
                source,
                summary: summary.clone(),
                tags,
                confidence,
                observed_at: None,
            })?;
            println!("Signal recorded: {summary}");
        }
        Command::Signal(SignalCmd::Analyze { text, tags, commit }) => {
            let proposals = FakeAgent.analyze_signal(SignalAnalysisInput {
                text,
                hints: tags,
            })?;
            if proposals.is_empty() {
                println!("Agent proposed no events.");
            } else {
                print_proposals(&proposals);
                if commit {
                    let mut rt = WorldRuntime::open_dir(&cli.dir)?;
                    for p in &proposals {
                        rt.ingest(WorldEvent::SignalObserved {
                            source: p.source.clone(),
                            summary: p.summary.clone(),
                            tags: p.tags.clone(),
                            confidence: p.confidence,
                            observed_at: None,
                        })?;
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
        Command::Explain(ExplainCmd::Latest) => match load_last_advance(&cli.dir)? {
            Some(e) => print!("{e}"),
            None => println!("No advance has been recorded yet. Run `liferuntime advance`."),
        },
        Command::Replay => {
            let rt = WorldRuntime::open_dir(&cli.dir)?;
            println!("Replayed {} events from {}.", rt.event_count(), cli.dir.display());
        }
        Command::Pulse { at } => {
            let mut rt = WorldRuntime::open_dir(&cli.dir)?;
            let observed_at = at.unwrap_or_else(Utc::now);
            rt.ingest(WorldEvent::TimePulseObserved { observed_at })?;
            println!("Pulse recorded at {observed_at}");
        }
    }
    Ok(())
}

fn cmd_init(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    let events = dir.join("events.jsonl");
    if !events.exists() {
        std::fs::File::create(&events)
            .with_context(|| format!("creating {}", events.display()))?;
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
    println!("  strategic_relevance: {:.2}", p.strategic_relevance);
    println!("  urgency:             {:.2}", p.urgency);
}

fn print_proposals(proposals: &[ProposedEvent]) {
    println!("Proposed by agent:");
    for (i, p) in proposals.iter().enumerate() {
        println!("  [{}] {} (confidence {:.2})", i + 1, p.summary, p.confidence);
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
    std::fs::write(&path, bytes)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn load_last_advance(dir: &Path) -> Result<Option<Explanation>> {
    let path = dir.join("last_advance.json");
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    Ok(Some(serde_json::from_slice(&bytes)?))
}
