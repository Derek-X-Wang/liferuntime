use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use liferuntime_world::{Explanation, ProjectView, WorldChanges, WorldEvent, WorldRuntime};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "liferuntime",
    about = "Persistent world simulation runtime",
    version
)]
struct Cli {
    /// Directory where events.jsonl, cursor.json, and last_advance.json live.
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
}

#[derive(Subcommand)]
enum SignalCmd {
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
        Command::Advance => {
            let mut rt = WorldRuntime::open_dir(&cli.dir)?;
            let changes = rt.advance()?;
            print_advance(&cli.dir, &changes)?;
        }
        Command::Inspect(InspectCmd::Project { id }) => {
            let mut rt = WorldRuntime::open_dir(&cli.dir)?;
            rt.materialize()?;
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
    println!("Project: {} ({})", p.name, p.id);
    println!("Tags: [{}]", p.tags.join(", "));
    println!("  strategic_relevance: {:.2}", p.strategic_relevance);
    println!("  urgency:             {:.2}", p.urgency);
    println!("  momentum:            {:.2}", p.momentum);
    println!("  maintenance_burden:  {:.2}", p.maintenance_burden);
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

