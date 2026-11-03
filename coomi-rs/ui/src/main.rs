use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use clap::Parser;
use coomi_catalogs::CatalogInstaller;
use coomi_catalogs::builtin_mcp;
use coomi_catalogs::builtin_skills;
use coomi_engine::Agent;
use coomi_engine::AgentEvent;
use coomi_engine::AgentObserver;
use coomi_engine::ApprovalHandler;
use coomi_engine::Session;
use coomi_engine::SessionStore;
use coomi_engine::TokenUsage;
use coomi_engine::ToolCall;
use coomi_security::AccessMode;
use coomi_security::HookRunner;
use coomi_security::SecurityPolicy;
use coomi_services::HttpModelProvider;
use coomi_services::McpRuntime;
use coomi_services::MemoryManager;
use coomi_services::ProviderConfig;
use coomi_services::ProviderRegistry;
use coomi_services::list_installed_skills;
use coomi_tools::AgentScheduler;
use coomi_tools::CoreTools;
use std::collections::BTreeMap;
use std::env;
use std::io;
use std::io::IsTerminal;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

mod clipboard;
mod terminal_ui;

#[derive(Debug, Parser)]
#[command(
    name = "coomi",
    version,
    about = "Coomi terminal coding agent",
    subcommand_negates_reqs = true
)]
struct Cli {
    /// Coomi data directory. Defaults to COOMI_HOME or ~/.coomi.
    #[arg(long, global = true)]
    home: Option<PathBuf>,

    /// Working directory used by the agent and tools.
    #[arg(long, global = true, default_value = ".")]
    cwd: PathBuf,

    /// Provider or provider:model selector from providers.json.
    #[arg(short, long, global = true)]
    model: Option<String>,

    /// File and process access policy.
    #[arg(long, global = true, value_enum, default_value = "workspace-write")]
    policy: AccessMode,

    /// Approve tool actions that would otherwise prompt.
    #[arg(short = 'y', long, global = true)]
    yes: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, clap::Subcommand)]
enum Command {
    /// Run one non-interactive agent turn.
    Exec {
        #[arg(trailing_var_arg = true)]
        prompt: Vec<String>,
    },
    /// List every model declared in providers.json.
    Models,
    /// List saved sessions.
    Sessions {
        /// Include sessions from other working directories.
        #[arg(long)]
        all: bool,
    },
    /// Resume a saved session, interactively or with one prompt.
    Resume {
        /// Session UUID. Omit with --last to resume the latest session.
        id: Option<Uuid>,
        #[arg(long)]
        last: bool,
        #[arg(long)]
        prompt: Option<String>,
    },
    /// Compact a saved session without running another agent turn.
    Compact {
        /// Session UUID. Omit to compact the latest workspace session.
        id: Option<Uuid>,
        /// Explicitly select the latest workspace session.
        #[arg(long)]
        last: bool,
    },
    /// Browse and install built-in MCP and Skill entries.
    Catalog {
        #[command(subcommand)]
        command: CatalogCommand,
    },
}

#[derive(Debug, clap::Subcommand)]
enum CatalogCommand {
    /// List built-in entries.
    List {
        #[arg(value_enum)]
        kind: CatalogKind,
    },
    /// Install one built-in entry.
    Install {
        #[arg(value_enum)]
        kind: CatalogKind,
        id: String,
        /// MCP template value in key=value form. Repeat for multiple values.
        #[arg(long = "set", value_name = "KEY=VALUE")]
        values: Vec<String>,
    },
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum CatalogKind {
    Mcp,
    Skill,
}

struct RuntimePaths {
    home: PathBuf,
    cwd: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = resolve_paths(&cli)?;
    match &cli.command {
        Some(Command::Models) => print_models(&load_registry(&paths.home)?),
        Some(Command::Sessions { all }) => print_sessions(
            &SessionStore::new(&paths.home),
            (!all).then_some(&paths.cwd),
        )?,
        Some(Command::Catalog { command }) => run_catalog(command, &paths.home)?,
        Some(Command::Exec { prompt }) => {
            let prompt = if prompt.is_empty() {
                read_stdin_prompt()?
            } else {
                prompt.join(" ")
            };
            anyhow::ensure!(!prompt.trim().is_empty(), "prompt must not be empty");
            let registry = load_registry(&paths.home)?;
            let provider = registry.resolve(cli.model.as_deref())?;
            let mut session = Session::new(&provider.id, &provider.model, paths.cwd.clone());
            run_turn(&cli, &paths, &mut session, provider, prompt, false).await?;
        }
        Some(Command::Resume { id, last, prompt }) => {
            anyhow::ensure!(
                !(*last && id.is_some()),
                "--last cannot be combined with ID"
            );
            let store = SessionStore::new(&paths.home);
            let mut session = if let Some(id) = id {
                store.load(*id)?
            } else {
                store
                    .latest(Some(&paths.cwd))?
                    .context("no session is available for this working directory")?
            };
            session.cwd = paths.cwd.clone();
            if let Some(prompt) = prompt {
                let registry = load_registry(&paths.home)?;
                let provider = provider_for_session(&registry, &session, cli.model.as_deref())?;
                run_turn(&cli, &paths, &mut session, provider, prompt.clone(), false).await?;
            } else {
                interactive(&cli, &paths, session).await?;
            }
        }
        Some(Command::Compact { id, last }) => {
            anyhow::ensure!(
                !(*last && id.is_some()),
                "--last cannot be combined with ID"
            );
            let store = SessionStore::new(&paths.home);
            let mut session = if let Some(id) = id {
                store.load(*id)?
            } else {
                store
                    .latest(Some(&paths.cwd))?
                    .context("no session is available for this working directory")?
            };
            session.cwd = paths.cwd.clone();
            let registry = load_registry(&paths.home)?;
            let provider = provider_for_session(&registry, &session, cli.model.as_deref())?;
            compact_session(&cli, &paths, &mut session, provider).await?;
        }
        None => {
            let registry = load_registry(&paths.home)?;
            let provider = registry.resolve(cli.model.as_deref())?;
            let session = Session::new(&provider.id, &provider.model, paths.cwd.clone());
            interactive(&cli, &paths, session).await?;
        }
    }
    Ok(())
}

async fn interactive(cli: &Cli, paths: &RuntimePaths, session: Session) -> Result<()> {
    terminal_ui::run(cli, paths, session).await
}

async fn run_turn(
    cli: &Cli,
    paths: &RuntimePaths,
    session: &mut Session,
    provider_config: ProviderConfig,
    prompt: String,
    interactive: bool,
) -> Result<()> {
    let policy = SecurityPolicy::new(&paths.cwd, cli.policy)?;
    let scheduler = AgentScheduler::new(
        paths.cwd.clone(),
        paths.home.clone(),
        provider_config.clone(),
        cli.policy,
        system_prompt(
            &paths.cwd,
            cli.policy,
            &coomi_engine::discover_project_instructions(&paths.cwd)?,
            &paths.home,
        ),
    );
    let tools = CoreTools::new(paths.cwd.clone(), policy)
        .with_skills_directory(paths.home.join("skills"))
        .with_config_home(paths.home.clone())
        .with_session_state(session.plan.clone(), session.loop_state.clone())
        .with_mcp_runtime(Arc::new(McpRuntime::load(&paths.home).await))
        .with_memory(Arc::new(MemoryManager::new(&paths.home, &paths.cwd)))
        .with_hooks(Arc::new(HookRunner::load(&paths.home)?))
        .with_file_history(paths.home.clone(), session.id.to_string())
        .with_agent_scheduler(scheduler, session.messages.clone());
    let provider = HttpModelProvider::new(provider_config)?;
    let instructions = coomi_engine::discover_project_instructions(&paths.cwd)?;
    let system_prompt = system_prompt(&paths.cwd, cli.policy, &instructions, &paths.home);
    let approval = TerminalApproval {
        interactive,
        approve_all: cli.yes,
    };
    let checkpoint_home = paths.home.clone();
    let agent = Agent::new(system_prompt).with_durable_checkpoint(Arc::new(move |session| {
        SessionStore::new(&checkpoint_home)
            .save(session)
            .map_err(|error| format!("{error:#}"))
    }));
    agent
        .run_turn(
            session,
            prompt,
            &provider,
            &tools,
            &approval,
            &TerminalObserver,
        )
        .await?;
    SessionStore::new(&paths.home).save(session)?;
    while session
        .loop_state
        .as_ref()
        .is_some_and(|state| state.status == coomi_engine::LoopStatus::Active)
    {
        agent
            .continue_loop(session, &provider, &tools, &approval, &TerminalObserver)
            .await?;
        SessionStore::new(&paths.home).save(session)?;
    }
    Ok(())
}

async fn compact_session(
    cli: &Cli,
    paths: &RuntimePaths,
    session: &mut Session,
    provider_config: ProviderConfig,
) -> Result<()> {
    let policy = SecurityPolicy::new(&paths.cwd, cli.policy)?;
    let instructions = coomi_engine::discover_project_instructions(&paths.cwd)?;
    let prompt = system_prompt(&paths.cwd, cli.policy, &instructions, &paths.home);
    let scheduler = AgentScheduler::new(
        paths.cwd.clone(),
        paths.home.clone(),
        provider_config.clone(),
        cli.policy,
        prompt.clone(),
    );
    let tools = CoreTools::new(paths.cwd.clone(), policy)
        .with_skills_directory(paths.home.join("skills"))
        .with_config_home(paths.home.clone())
        .with_session_state(session.plan.clone(), session.loop_state.clone())
        .with_mcp_runtime(Arc::new(McpRuntime::load(&paths.home).await))
        .with_memory(Arc::new(MemoryManager::new(&paths.home, &paths.cwd)))
        .with_hooks(Arc::new(HookRunner::load(&paths.home)?))
        .with_file_history(paths.home.clone(), session.id.to_string())
        .with_agent_scheduler(scheduler, session.messages.clone());
    let provider = HttpModelProvider::new(provider_config)?;
    Agent::new(prompt)
        .compact_session(session, &provider, &tools, &TerminalObserver)
        .await?;
    SessionStore::new(&paths.home).save(session)?;
    println!("compacted session {}", session.id);
    Ok(())
}

fn system_prompt(cwd: &Path, policy: AccessMode, instructions: &str, home: &Path) -> String {
    let skills = list_installed_skills(home)
        .unwrap_or_default()
        .into_iter()
        .filter(|skill| skill.enabled)
        .map(|skill| skill.name)
        .collect::<Vec<_>>();
    let mcp = installed_mcp_names(&home.join("config").join("mcp_servers.json"));
    let mut prompt = format!(
        "You are Coomi, a pragmatic terminal coding agent. Work directly in the user's project. \
Use tools to inspect evidence before editing. Keep changes scoped, preserve unrelated work, and \
verify implementation results. Never invent tool results.\n\nWorking directory: {}\nAccess policy: {}",
        cwd.display(),
        policy.label()
    );
    prompt.push_str(
        "\nWhen the user asks to install, configure, or repair an MCP server or Skill, use the dedicated configure_mcp or install_skill tool. Diagnose failing commands first, then update the smallest configuration necessary; do not ask the user to edit Coomi JSON manually.",
    );
    if !skills.is_empty() {
        prompt.push_str(&format!("\nInstalled skills: {}", skills.join(", ")));
    }
    if !mcp.is_empty() {
        prompt.push_str(&format!("\nConfigured MCP servers: {}", mcp.join(", ")));
    }
    let memory = MemoryManager::new(home, cwd).prompt_context();
    if !memory.is_empty() {
        prompt.push_str("\n\nPersistent memory (local overrides project and global):\n");
        prompt.push_str(&memory);
    }
    if !instructions.trim().is_empty() {
        prompt.push_str("\n\nProject instructions:\n");
        prompt.push_str(instructions);
    }
    prompt
}

fn resolve_paths(cli: &Cli) -> Result<RuntimePaths> {
    let home = cli
        .home
        .clone()
        .or_else(|| env::var_os("COOMI_HOME").map(PathBuf::from))
        .or_else(|| dirs::home_dir().map(|path| path.join(".coomi")))
        .context("could not determine Coomi home directory")?;
    std::fs::create_dir_all(&home)
        .with_context(|| format!("failed to create Coomi home {}", home.display()))?;
    let home = home.canonicalize()?;
    let cwd = cli
        .cwd
        .canonicalize()
        .with_context(|| format!("invalid working directory {}", cli.cwd.display()))?;
    Ok(RuntimePaths { home, cwd })
}

fn load_registry(home: &Path) -> Result<ProviderRegistry> {
    let path = home.join("config").join("providers.json");
    ProviderRegistry::load(&path).with_context(|| {
        format!(
            "unable to load models from {}; configure at least one provider first",
            path.display()
        )
    })
}

fn provider_for_session(
    registry: &ProviderRegistry,
    session: &Session,
    override_selector: Option<&str>,
) -> Result<ProviderConfig> {
    if let Some(selector) = override_selector {
        return registry.resolve(Some(selector));
    }
    registry.resolve(Some(&format!("{}:{}", session.provider_id, session.model)))
}

fn print_models(registry: &ProviderRegistry) {
    for (index, choice) in registry.choices().iter().enumerate() {
        let active = if choice.provider_id == registry.active_id() && !choice.is_fast {
            " *"
        } else {
            ""
        };
        let mode = if choice.is_fast { " [fast]" } else { "" };
        println!(
            "{:>2}. {:<24} {} / {}{}{}",
            index + 1,
            choice.selector,
            choice.provider_display,
            choice.model,
            mode,
            active
        );
    }
}

fn print_sessions(store: &SessionStore, cwd: Option<&Path>) -> Result<()> {
    let sessions = store.list(cwd)?;
    if sessions.is_empty() {
        println!("no sessions");
        return Ok(());
    }
    for session in sessions {
        println!(
            "{}  {}  {}/{}  {}",
            session.id,
            session.updated_at.format("%Y-%m-%d %H:%M"),
            session.provider_id,
            session.model,
            if session.title.is_empty() {
                session.preview
            } else {
                session.title
            }
        );
    }
    Ok(())
}

fn run_catalog(command: &CatalogCommand, home: &Path) -> Result<()> {
    match command {
        CatalogCommand::List { kind } => list_catalog(*kind),
        CatalogCommand::Install { kind, id, values } => {
            let installer = CatalogInstaller::new(home);
            let path = match kind {
                CatalogKind::Mcp => {
                    let values = parse_assignments(values)?;
                    installer.install_mcp(id, &values)?
                }
                CatalogKind::Skill => installer.install_skill(id)?,
            };
            println!("installed {} at {}", id, path.display());
            Ok(())
        }
    }
}

fn list_catalog(kind: CatalogKind) -> Result<()> {
    match kind {
        CatalogKind::Mcp => {
            for entry in builtin_mcp()?.entries {
                println!("{:<20} {:<24} {}", entry.id, entry.name, entry.description);
            }
        }
        CatalogKind::Skill => {
            for entry in builtin_skills()?.entries {
                println!("{:<20} {:<28} {}", entry.id, entry.name, entry.description);
            }
        }
    }
    Ok(())
}

fn parse_assignments(values: &[String]) -> Result<BTreeMap<String, String>> {
    values
        .iter()
        .map(|value| {
            let (key, value) = value
                .split_once('=')
                .with_context(|| format!("expected key=value, got `{value}`"))?;
            anyhow::ensure!(!key.trim().is_empty(), "parameter key must not be empty");
            Ok((key.trim().to_string(), value.to_string()))
        })
        .collect()
}

fn installed_mcp_names(path: &Path) -> Vec<String> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Vec::new();
    };
    let mut names = value
        .get("servers")
        .and_then(serde_json::Value::as_object)
        .map(|servers| {
            servers
                .iter()
                .filter(|(_, server)| {
                    server
                        .get("enabled")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(true)
                })
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn read_stdin_prompt() -> Result<String> {
    if io::stdin().is_terminal() {
        anyhow::bail!("prompt is required when stdin is a terminal")
    }
    let mut prompt = String::new();
    io::stdin().read_to_string(&mut prompt)?;
    Ok(prompt)
}

struct TerminalObserver;

impl AgentObserver for TerminalObserver {
    fn on_event(&self, event: &AgentEvent) {
        match event {
            AgentEvent::ModelStarted { round, .. } if *round > 1 => {
                eprintln!("[model round {round}]");
            }
            AgentEvent::Text(text) => println!("\n{text}"),
            AgentEvent::TextDelta(text) => print!("{text}"),
            AgentEvent::ReasoningDelta(text) => eprint!("{text}"),
            AgentEvent::ContextUpdated(status) => eprintln!(
                "[context {}% {}/{}]",
                status.used_percent, status.used_tokens, status.effective_context_window
            ),
            AgentEvent::CompactionStarted { automatic } => eprintln!(
                "[context compaction {}]",
                if *automatic { "automatic" } else { "manual" }
            ),
            AgentEvent::CompactionCompleted {
                before_tokens,
                after_tokens,
                ..
            } => eprintln!("[context compacted {before_tokens} -> {after_tokens}]"),
            AgentEvent::PlanUpdated(plan) => {
                eprintln!("[plan updated {} step(s)]", plan.steps.len())
            }
            AgentEvent::LoopUpdated(loop_state) => {
                eprintln!("[loop {:?}] {}", loop_state.status, loop_state.objective)
            }
            AgentEvent::QueuedInputAccepted(messages) => {
                eprintln!("[queued input accepted: {}]", messages.len())
            }
            AgentEvent::ToolStarted(call) => {
                eprintln!("[tool {}] {}", call.name, compact_json(&call.arguments));
            }
            AgentEvent::ToolFinished { call, result } => {
                let status = if result.success { "ok" } else { "error" };
                eprintln!("[tool {} {status}] {}", call.name, preview(&result.output));
            }
            AgentEvent::ConnectionRetry {
                attempt,
                max_attempts,
                delay_ms,
                message,
            } => eprintln!("[reconnect {attempt}/{max_attempts} in {delay_ms}ms] {message}"),
            AgentEvent::StreamReset => eprintln!("[stream reset]"),
            AgentEvent::ReasoningCompleted => {}
            AgentEvent::PersistenceFailed(error) => eprintln!("[persistence failed] {error}"),
            AgentEvent::ModelUsage { total, .. } => {
                eprintln!(
                    "[usage {} input / {} output]",
                    total.input_tokens, total.output_tokens
                )
            }
            AgentEvent::TurnCompleted { total, .. } => print_usage(total),
            AgentEvent::ModelStarted { .. } => {}
        }
    }
}

struct TerminalApproval {
    interactive: bool,
    approve_all: bool,
}

#[async_trait]
impl ApprovalHandler for TerminalApproval {
    async fn approve(&self, call: &ToolCall, reason: &str) -> bool {
        if self.approve_all {
            return true;
        }
        if !self.interactive {
            return false;
        }
        eprintln!("approval required: {reason}");
        eprintln!("tool: {} {}", call.name, compact_json(&call.arguments));
        eprint!("approve once? [y/N] ");
        if io::stderr().flush().is_err() {
            return false;
        }
        let mut answer = String::new();
        io::stdin()
            .read_line(&mut answer)
            .is_ok_and(|_| matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
    }
}

fn compact_json(value: &serde_json::Value) -> String {
    preview(&serde_json::to_string(value).unwrap_or_else(|_| "{}".into()))
}

fn preview(value: &str) -> String {
    let single_line = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut output = single_line.chars().take(180).collect::<String>();
    if single_line.chars().count() > 180 {
        output.push_str("...");
    }
    output
}

fn print_usage(usage: &TokenUsage) {
    eprintln!(
        "[usage input={} cached={} output={} total={}]",
        usage.input_tokens,
        usage.cached_input_tokens,
        usage.output_tokens,
        usage.total_tokens()
    );
}
