mod editor;
mod render;
mod theme;

use self::editor::Editor;
use super::Cli;
use super::RuntimePaths;
use super::load_registry;
use super::provider_for_session;
use super::system_prompt;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use coomi_catalogs::CatalogInstaller;
use coomi_catalogs::McpEntry;
use coomi_catalogs::RequiredParameter;
use coomi_catalogs::SkillEntry;
use coomi_catalogs::builtin_mcp;
use coomi_catalogs::builtin_skills;
use coomi_engine::Agent;
use coomi_engine::AgentEvent;
use coomi_engine::AgentObserver;
use coomi_engine::ApprovalHandler;
use coomi_engine::ContextStatus;
use coomi_engine::InputQueue;
use coomi_engine::Role;
use coomi_engine::Session;
use coomi_engine::SessionStore;
use coomi_engine::SessionSummary;
use coomi_engine::ToolCall;
use coomi_engine::UserInputRequest;
use coomi_engine::UserInputResponse;
use coomi_security::AccessMode;
use coomi_security::HookRunner;
use coomi_security::SecurityPolicy;
use coomi_services::AutoConfigIntent;
use coomi_services::AutoConfigResult;
use coomi_services::ConfiguredMcp;
use coomi_services::HttpModelProvider;
use coomi_services::InstalledSkill;
use coomi_services::McpRuntime;
use coomi_services::McpServerStatus;
use coomi_services::MemoryManager;
use coomi_services::MemoryScope;
use coomi_services::MemoryType;
use coomi_services::ModelChoice;
use coomi_services::ProviderDocument;
use coomi_services::ProviderSettings;
use coomi_services::RemoteCompactionMode;
use coomi_services::UpdateCheckResult;
use coomi_services::apply_auto_config;
use coomi_services::check_for_update;
use coomi_services::detect_auto_config;
use coomi_services::list_configured_mcp;
use coomi_services::list_installed_skills;
use coomi_services::remove_configured_mcp;
use coomi_services::remove_installed_skill;
use coomi_services::set_mcp_enabled;
use coomi_services::set_skill_enabled;
use coomi_services::update_installed_skill;
use coomi_tools::AgentScheduler;
use coomi_tools::CoreTools;
use crossterm::event;
use crossterm::event::DisableBracketedPaste;
use crossterm::event::DisableMouseCapture;
use crossterm::event::EnableBracketedPaste;
use crossterm::event::EnableMouseCapture;
use crossterm::event::Event;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use crossterm::event::MouseButton;
use crossterm::event::MouseEvent;
use crossterm::event::MouseEventKind;
use crossterm::execute;
use crossterm::terminal::EnterAlternateScreen;
use crossterm::terminal::LeaveAlternateScreen;
use crossterm::terminal::disable_raw_mode;
use crossterm::terminal::enable_raw_mode;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use serde_json::Value;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task::AbortHandle;
use uuid::Uuid;

const FRAME_INTERVAL: Duration = Duration::from_millis(70);
const DOUBLE_ESCAPE_WINDOW: Duration = Duration::from_millis(700);

pub async fn run(cli: &Cli, paths: &RuntimePaths, session: Session) -> Result<()> {
    enable_raw_mode().context("failed to enable terminal raw mode")?;
    let _restore = TerminalRestore;
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture
    )
    .context("failed to enter terminal UI")?;

    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend).context("failed to create terminal")?;
    terminal.clear()?;
    let mut app = TuiState::new(cli, paths, session)?;
    let (runtime_tx, mut runtime_rx) = mpsc::unbounded_channel();
    start_update_check(runtime_tx.clone());

    while !app.quit {
        while let Ok(runtime_event) = runtime_rx.try_recv() {
            handle_runtime_event(&mut app, runtime_event);
        }
        if !app.busy
            && !app.auto_config_busy
            && app.pending_approval.is_none()
            && app.pending_update.is_none()
        {
            if let Some(prompt) = app.queue.pop_front() {
                app.input_queue.discard_front(&prompt);
                start_agent_turn(&mut app, prompt, false, runtime_tx.clone());
            } else if app.loop_continuation_pending {
                app.loop_continuation_pending = false;
                start_agent_turn(&mut app, String::new(), true, runtime_tx.clone());
            }
        }

        terminal.draw(|frame| render::draw(frame, &app))?;

        if event::poll(FRAME_INTERVAL)? {
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    handle_key(&mut app, key, runtime_tx.clone())?;
                }
                Event::Paste(text) if app.accepts_text_input() => {
                    app.active_editor_mut().insert_str(&text);
                }
                Event::Mouse(mouse) => handle_mouse(&mut app, mouse),
                _ => {}
            }
        }
        app.spinner_tick = app.spinner_tick.wrapping_add(1);
    }

    if let Some(abort) = app.active_abort.take() {
        abort.abort();
    }
    if let Some(mut approval) = app.pending_approval.take()
        && let Some(responder) = approval.responder.take()
    {
        let _ = responder.send(false);
    }
    if let Some(mut pending) = app.pending_user_input.take()
        && let Some(responder) = pending.responder.take()
    {
        let _ = responder.send(None);
    }
    Ok(())
}

struct TerminalRestore;

impl Drop for TerminalRestore {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NoticeKind {
    Success,
    Warning,
    Error,
}

#[derive(Clone, Debug)]
enum ToolState {
    Running,
    Complete { success: bool, output: String },
}

#[derive(Clone, Debug)]
enum TimelineEntry {
    User(String),
    Assistant(String),
    Reasoning(String),
    SideUser(String),
    SideAssistant(String),
    Tool {
        id: String,
        name: String,
        arguments: Value,
        state: ToolState,
    },
    Notice {
        kind: NoticeKind,
        text: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OverlayKind {
    Commands,
    Models,
    History,
    Catalog,
    Help,
    McpConfig,
    Settings,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CatalogTab {
    Mcp,
    Skills,
}

#[derive(Debug)]
struct Overlay {
    kind: OverlayKind,
    selected: usize,
    query: Editor,
    catalog_tab: CatalogTab,
}

impl Overlay {
    fn new(kind: OverlayKind) -> Self {
        Self {
            kind,
            selected: 0,
            query: Editor::default(),
            catalog_tab: CatalogTab::Mcp,
        }
    }
}

struct PendingApproval {
    call: ToolCall,
    reason: String,
    selected: ApprovalChoice,
    responder: Option<oneshot::Sender<bool>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UpdateChoice {
    Yes,
    No,
}

impl UpdateChoice {
    fn toggle(self) -> Self {
        match self {
            Self::Yes => Self::No,
            Self::No => Self::Yes,
        }
    }
}

struct PendingUpdate {
    current_version: String,
    latest_version: String,
    release_url: String,
    selected: UpdateChoice,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ApprovalChoice {
    Approve,
    Deny,
}

impl ApprovalChoice {
    fn toggle(self) -> Self {
        match self {
            Self::Approve => Self::Deny,
            Self::Deny => Self::Approve,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PlanModeChoice {
    ExitAndSend,
    SendReadOnly,
    Cancel,
}

impl PlanModeChoice {
    fn next(self) -> Self {
        match self {
            Self::ExitAndSend => Self::SendReadOnly,
            Self::SendReadOnly => Self::Cancel,
            Self::Cancel => Self::ExitAndSend,
        }
    }
    fn prev(self) -> Self {
        match self {
            Self::ExitAndSend => Self::Cancel,
            Self::SendReadOnly => Self::ExitAndSend,
            Self::Cancel => Self::SendReadOnly,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::ExitAndSend => "Yes, exit plan mode and send",
            Self::SendReadOnly => "No, send as read-only side session",
            Self::Cancel => "Cancel",
        }
    }
}

struct PlanModeConfirm {
    prompt: String,
    selected: PlanModeChoice,
}

struct PendingUserInput {
    request: UserInputRequest,
    question_index: usize,
    option_index: usize,
    other_editor: Option<Editor>,
    answers: UserInputResponse,
    responder: Option<oneshot::Sender<Option<UserInputResponse>>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum DeleteTarget {
    Session(Uuid),
    Provider(String),
    Mcp(String),
    Skill(String),
}

struct McpForm {
    entry: McpEntry,
    fields: Vec<(RequiredParameter, Editor)>,
    selected: usize,
}

#[derive(Clone, Copy)]
struct ProviderOption {
    value: &'static str,
    label: &'static str,
}

#[derive(Clone, Copy)]
struct ProviderField {
    key: &'static str,
    label: &'static str,
    secret: bool,
    options: &'static [ProviderOption],
}

impl ProviderField {
    fn is_choice(self) -> bool {
        !self.options.is_empty()
    }

    fn option_label(self, value: &str) -> &str {
        self.options
            .iter()
            .find(|option| option.value == value)
            .map_or(value, |option| option.label)
    }

    fn is_custom_input(self, value: &str) -> bool {
        self.key == "context_window"
            && value != CUSTOM_CONTEXT_WINDOW
            && !self.options.iter().any(|option| option.value == value)
    }
}

const TEXT_FIELD: &[ProviderOption] = &[];
const PROVIDER_TYPE_OPTIONS: &[ProviderOption] = &[
    ProviderOption {
        value: "openai_compatible",
        label: "OpenAI Compatible",
    },
    ProviderOption {
        value: "openai_responses",
        label: "OpenAI Responses",
    },
    ProviderOption {
        value: "anthropic_messages",
        label: "Anthropic Messages",
    },
    ProviderOption {
        value: "gemini_native",
        label: "Google Gemini (Native)",
    },
];
const TOOL_PROTOCOL_OPTIONS: &[ProviderOption] = &[
    ProviderOption {
        value: "auto",
        label: "Auto (recommended)",
    },
    ProviderOption {
        value: "native",
        label: "Native tool calling",
    },
    ProviderOption {
        value: "structured",
        label: "Structured text",
    },
    ProviderOption {
        value: "mimo",
        label: "MiMo text tools",
    },
    ProviderOption {
        value: "disabled",
        label: "Disabled",
    },
];
const CUSTOM_CONTEXT_WINDOW: &str = "__custom__";
const CONTEXT_WINDOW_OPTIONS: &[ProviderOption] = &[
    ProviderOption {
        value: "128000",
        label: "128K",
    },
    ProviderOption {
        value: "256000",
        label: "256K (default)",
    },
    ProviderOption {
        value: "512000",
        label: "512K",
    },
    ProviderOption {
        value: CUSTOM_CONTEXT_WINDOW,
        label: "Custom...",
    },
];
const COMPACT_SCOPE_OPTIONS: &[ProviderOption] = &[
    ProviderOption {
        value: "total",
        label: "Total context",
    },
    ProviderOption {
        value: "body_after_prefix",
        label: "Body after prefix",
    },
];
const OPTIONAL_BOOL_OPTIONS: &[ProviderOption] = &[
    ProviderOption {
        value: "",
        label: "Auto",
    },
    ProviderOption {
        value: "true",
        label: "Enabled",
    },
    ProviderOption {
        value: "false",
        label: "Disabled",
    },
];
const BOOL_OPTIONS: &[ProviderOption] = &[
    ProviderOption {
        value: "true",
        label: "Enabled",
    },
    ProviderOption {
        value: "false",
        label: "Disabled",
    },
];
const REMOTE_MODE_OPTIONS: &[ProviderOption] = &[
    ProviderOption {
        value: "v2",
        label: "V2",
    },
    ProviderOption {
        value: "legacy",
        label: "Legacy",
    },
];

const PROVIDER_FIELDS: &[ProviderField] = &[
    ProviderField {
        key: "id",
        label: "Provider ID",
        secret: false,
        options: TEXT_FIELD,
    },
    ProviderField {
        key: "display",
        label: "Display name",
        secret: false,
        options: TEXT_FIELD,
    },
    ProviderField {
        key: "type",
        label: "API compatibility",
        secret: false,
        options: PROVIDER_TYPE_OPTIONS,
    },
    ProviderField {
        key: "tool_protocol",
        label: "Tool calling",
        secret: false,
        options: TOOL_PROTOCOL_OPTIONS,
    },
    ProviderField {
        key: "base_url",
        label: "Base URL",
        secret: false,
        options: TEXT_FIELD,
    },
    ProviderField {
        key: "model",
        label: "Model",
        secret: false,
        options: TEXT_FIELD,
    },
    ProviderField {
        key: "fast_model",
        label: "Fast model",
        secret: false,
        options: TEXT_FIELD,
    },
    ProviderField {
        key: "api_key",
        label: "API Key",
        secret: true,
        options: TEXT_FIELD,
    },
    ProviderField {
        key: "context_window",
        label: "Context window",
        secret: false,
        options: CONTEXT_WINDOW_OPTIONS,
    },
    ProviderField {
        key: "effective_context_window_percent",
        label: "Effective window %",
        secret: false,
        options: TEXT_FIELD,
    },
    ProviderField {
        key: "auto_compact_token_limit",
        label: "Compact token limit",
        secret: false,
        options: TEXT_FIELD,
    },
    ProviderField {
        key: "auto_compact_scope",
        label: "Compact scope",
        secret: false,
        options: COMPACT_SCOPE_OPTIONS,
    },
    ProviderField {
        key: "max_output_tokens",
        label: "Max output tokens",
        secret: false,
        options: TEXT_FIELD,
    },
    ProviderField {
        key: "supports_remote_compaction",
        label: "Remote compaction",
        secret: false,
        options: OPTIONAL_BOOL_OPTIONS,
    },
    ProviderField {
        key: "remote_compaction_mode",
        label: "Remote mode",
        secret: false,
        options: REMOTE_MODE_OPTIONS,
    },
    ProviderField {
        key: "supports_vision",
        label: "Vision",
        secret: false,
        options: BOOL_OPTIONS,
    },
    ProviderField {
        key: "supports_native_tools",
        label: "Native tools",
        secret: false,
        options: BOOL_OPTIONS,
    },
    ProviderField {
        key: "supports_web_search",
        label: "Native web search",
        secret: false,
        options: BOOL_OPTIONS,
    },
    ProviderField {
        key: "supports_parallel_tool_calls",
        label: "Parallel tools",
        secret: false,
        options: BOOL_OPTIONS,
    },
];

struct ProviderForm {
    original_id: Option<String>,
    fields: Vec<(ProviderField, Editor)>,
    selected: usize,
    choice_open: bool,
    show_secret: bool,
}

struct SettingsState {
    tab: SettingsTab,
    document: ProviderDocument,
    provider_ids: Vec<String>,
    mcp_servers: Vec<ConfiguredMcp>,
    mcp_statuses: Vec<McpServerStatus>,
    skills: Vec<InstalledSkill>,
    selected: usize,
    show_secret: bool,
    form: Option<ProviderForm>,
    error: Option<String>,
}

#[derive(Clone)]
struct SettingsMcpItem {
    entry: Option<McpEntry>,
    configured: Option<ConfiguredMcp>,
}

impl SettingsMcpItem {
    fn id(&self) -> &str {
        self.entry
            .as_ref()
            .map(|entry| entry.id.as_str())
            .or_else(|| self.configured.as_ref().map(|item| item.name.as_str()))
            .unwrap_or_default()
    }
}

#[derive(Clone)]
struct SettingsSkillItem {
    entry: Option<SkillEntry>,
    installed: Option<InstalledSkill>,
}

impl SettingsSkillItem {
    fn id(&self) -> &str {
        self.entry
            .as_ref()
            .map(|entry| entry.id.as_str())
            .or_else(|| self.installed.as_ref().map(|item| item.name.as_str()))
            .unwrap_or_default()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SettingsTab {
    Providers,
    Mcp,
    Skills,
    Runtime,
}

impl SettingsTab {
    fn next(self) -> Self {
        match self {
            Self::Providers => Self::Mcp,
            Self::Mcp => Self::Skills,
            Self::Skills => Self::Runtime,
            Self::Runtime => Self::Providers,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::Providers => Self::Runtime,
            Self::Mcp => Self::Providers,
            Self::Skills => Self::Mcp,
            Self::Runtime => Self::Skills,
        }
    }
}

#[derive(Clone, Copy)]
enum CommandAction {
    NewSession,
    History,
    Models,
    Status,
    Compact,
    Catalog,
    Mcp,
    Skills,
    Memory,
    Plan,
    ExitPlan,
    Loop,
    Settings,
    ClearTimeline,
    Help,
    Quit,
}

struct CommandItem {
    label: &'static str,
    detail: &'static str,
    action: CommandAction,
}

const COMMANDS: &[CommandItem] = &[
    CommandItem {
        label: "New session",
        detail: "Start with an empty conversation",
        action: CommandAction::NewSession,
    },
    CommandItem {
        label: "Session history",
        detail: "Resume or remove a previous session",
        action: CommandAction::History,
    },
    CommandItem {
        label: "Switch model",
        detail: "Choose from providers.json",
        action: CommandAction::Models,
    },
    CommandItem {
        label: "Session status",
        detail: "Show model, policy, context, Plan, and Loop",
        action: CommandAction::Status,
    },
    CommandItem {
        label: "Compact context",
        detail: "Compress this session now",
        action: CommandAction::Compact,
    },
    CommandItem {
        label: "Catalogs",
        detail: "Browse installable MCP and Skills",
        action: CommandAction::Catalog,
    },
    CommandItem {
        label: "MCP servers",
        detail: "Manage configured MCP servers",
        action: CommandAction::Mcp,
    },
    CommandItem {
        label: "Skills",
        detail: "Manage installed Skills",
        action: CommandAction::Skills,
    },
    CommandItem {
        label: "Memory",
        detail: "List and manage persistent memories",
        action: CommandAction::Memory,
    },
    CommandItem {
        label: "Enter plan mode",
        detail: "Switch to read-only plan mode",
        action: CommandAction::Plan,
    },
    CommandItem {
        label: "Exit plan mode",
        detail: "Return to the previous policy",
        action: CommandAction::ExitPlan,
    },
    CommandItem {
        label: "Loop",
        detail: "Create or control a persistent objective",
        action: CommandAction::Loop,
    },
    CommandItem {
        label: "Settings",
        detail: "Manage providers and runtime configuration",
        action: CommandAction::Settings,
    },
    CommandItem {
        label: "Clear timeline",
        detail: "Keep the session but clear this view",
        action: CommandAction::ClearTimeline,
    },
    CommandItem {
        label: "Keyboard help",
        detail: "Show the shortcut reference",
        action: CommandAction::Help,
    },
    CommandItem {
        label: "Quit Coomi",
        detail: "Close the terminal UI",
        action: CommandAction::Quit,
    },
];

struct TuiState {
    home: PathBuf,
    cwd: PathBuf,
    session: Session,
    timeline: Vec<TimelineEntry>,
    editor: Editor,
    input_history: Vec<String>,
    history_cursor: Option<usize>,
    queue: VecDeque<String>,
    input_queue: Arc<InputQueue>,
    models: Vec<ModelChoice>,
    sessions: Vec<SessionSummary>,
    mcp_entries: Vec<McpEntry>,
    skill_entries: Vec<SkillEntry>,
    overlay: Option<Overlay>,
    pending_approval: Option<PendingApproval>,
    pending_user_input: Option<PendingUserInput>,
    pending_update: Option<PendingUpdate>,
    confirm_delete: Option<DeleteTarget>,
    mcp_form: Option<McpForm>,
    settings: Option<SettingsState>,
    policy: AccessMode,
    approve_all: bool,
    busy: bool,
    active_generation: Option<u64>,
    next_generation: u64,
    active_abort: Option<AbortHandle>,
    catalog_busy: Option<String>,
    auto_config_busy: bool,
    settings_busy: bool,
    update_status: String,
    status: String,
    context_status: ContextStatus,
    scroll: u16,
    timeline_max_scroll: Cell<u16>,
    follow_tail: bool,
    scrollbar_area: Cell<Option<Rect>>,
    scrollbar_dragging: bool,
    spinner_tick: usize,
    last_escape: Option<Instant>,
    loop_continuation_pending: bool,
    side_busy: bool,
    plan_mode: bool,
    previous_policy: Option<AccessMode>,
    plan_mode_confirm: Option<PlanModeConfirm>,
    quit: bool,
}

impl TuiState {
    fn new(cli: &Cli, paths: &RuntimePaths, session: Session) -> Result<Self> {
        let registry = load_registry(&paths.home)?;
        let sessions = SessionStore::new(&paths.home).list(Some(&paths.cwd))?;
        let loop_continuation_pending = session
            .loop_state
            .as_ref()
            .is_some_and(|state| state.status == coomi_engine::LoopStatus::Active);
        let mut state = Self {
            home: paths.home.clone(),
            cwd: paths.cwd.clone(),
            session,
            timeline: Vec::new(),
            editor: Editor::default(),
            input_history: Vec::new(),
            history_cursor: None,
            queue: VecDeque::new(),
            input_queue: Arc::new(InputQueue::default()),
            models: registry.choices(),
            sessions,
            mcp_entries: builtin_mcp()?.entries,
            skill_entries: builtin_skills()?.entries,
            overlay: None,
            pending_approval: None,
            pending_user_input: None,
            pending_update: None,
            confirm_delete: None,
            mcp_form: None,
            settings: None,
            policy: cli.policy,
            approve_all: cli.yes,
            busy: false,
            active_generation: None,
            next_generation: 1,
            active_abort: None,
            catalog_busy: None,
            auto_config_busy: false,
            settings_busy: false,
            update_status: "Checking for updates".into(),
            status: "Ready".into(),
            context_status: ContextStatus::default(),
            scroll: 0,
            timeline_max_scroll: Cell::new(0),
            follow_tail: true,
            scrollbar_area: Cell::new(None),
            scrollbar_dragging: false,
            spinner_tick: 0,
            last_escape: None,
            loop_continuation_pending,
            side_busy: false,
            plan_mode: false,
            previous_policy: None,
            plan_mode_confirm: None,
            quit: false,
        };
        state.rebuild_timeline();
        Ok(state)
    }

    fn rebuild_timeline(&mut self) {
        self.timeline.clear();
        let mut tool_names = BTreeMap::new();
        for message in &self.session.messages {
            match message.role {
                Role::User if !message.internal => self
                    .timeline
                    .push(TimelineEntry::User(message.content.clone())),
                Role::User => {}
                Role::Assistant => {
                    if !message.content.is_empty() {
                        self.timeline
                            .push(TimelineEntry::Assistant(message.content.clone()));
                    }
                    for call in &message.tool_calls {
                        tool_names
                            .insert(call.id.clone(), (call.name.clone(), call.arguments.clone()));
                    }
                }
                Role::Tool => {
                    let call_id = message.tool_call_id.clone().unwrap_or_default();
                    let (name, arguments) = tool_names
                        .remove(&call_id)
                        .unwrap_or_else(|| ("tool".into(), Value::Object(Default::default())));
                    self.timeline.push(TimelineEntry::Tool {
                        id: call_id,
                        name,
                        arguments,
                        state: ToolState::Complete {
                            success: message.content.starts_with("success:"),
                            output: message.content.clone(),
                        },
                    });
                }
                Role::System => {}
            }
        }
        self.follow_tail = true;
    }

    fn push_notice(&mut self, kind: NoticeKind, text: impl Into<String>) {
        self.timeline.push(TimelineEntry::Notice {
            kind,
            text: text.into(),
        });
    }

    fn accepts_text_input(&self) -> bool {
        if self
            .pending_user_input
            .as_ref()
            .is_some_and(|pending| pending.other_editor.is_some())
        {
            return true;
        }
        self.pending_approval.is_none()
            && self.pending_user_input.is_none()
            && self.pending_update.is_none()
            && self.confirm_delete.is_none()
            && self
                .overlay
                .as_ref()
                .is_none_or(|overlay| overlay.kind != OverlayKind::Help)
    }

    fn active_editor_mut(&mut self) -> &mut Editor {
        if let Some(editor) = self
            .pending_user_input
            .as_mut()
            .and_then(|pending| pending.other_editor.as_mut())
        {
            return editor;
        }
        if let Some(form) = self
            .settings
            .as_mut()
            .and_then(|settings| settings.form.as_mut())
        {
            return &mut form.fields[form.selected].1;
        }
        if self
            .overlay
            .as_ref()
            .is_some_and(|overlay| overlay.kind == OverlayKind::McpConfig)
            && let Some(form) = &mut self.mcp_form
        {
            return &mut form.fields[form.selected].1;
        }
        if let Some(overlay) = &mut self.overlay {
            return &mut overlay.query;
        }
        &mut self.editor
    }

    fn refresh_sessions(&mut self) {
        match SessionStore::new(&self.home).list(Some(&self.cwd)) {
            Ok(sessions) => self.sessions = sessions,
            Err(error) => self.push_notice(NoticeKind::Error, error.to_string()),
        }
    }

    fn open_overlay(&mut self, kind: OverlayKind) {
        if kind == OverlayKind::History {
            self.refresh_sessions();
        }
        if kind == OverlayKind::Settings {
            match ProviderDocument::load(&self.home.join("config").join("providers.json")) {
                Ok(document) => {
                    let provider_ids = document.providers.keys().cloned().collect();
                    self.settings = Some(SettingsState {
                        tab: SettingsTab::Providers,
                        document,
                        provider_ids,
                        mcp_servers: list_configured_mcp(&self.home).unwrap_or_default(),
                        mcp_statuses: Vec::new(),
                        skills: list_installed_skills(&self.home).unwrap_or_default(),
                        selected: 0,
                        show_secret: false,
                        form: None,
                        error: None,
                    });
                }
                Err(error) => {
                    self.push_notice(NoticeKind::Error, error.to_string());
                    return;
                }
            }
        }
        self.overlay = Some(Overlay::new(kind));
    }

    fn close_overlay(&mut self) {
        self.overlay = None;
        self.mcp_form = None;
        self.settings = None;
    }

    fn scroll_up(&mut self, amount: u16) {
        let max_scroll = self.timeline_max_scroll.get();
        if max_scroll == 0 {
            self.scroll = 0;
            self.follow_tail = true;
            return;
        }
        let current = if self.follow_tail {
            max_scroll
        } else {
            self.scroll.min(max_scroll)
        };
        self.scroll = current.saturating_sub(amount);
        self.follow_tail = false;
    }

    fn scroll_down(&mut self, amount: u16) {
        let max_scroll = self.timeline_max_scroll.get();
        self.scroll = self.scroll.saturating_add(amount).min(max_scroll);
        self.follow_tail = self.scroll >= max_scroll;
    }

    fn cycle_policy(&mut self) {
        if self.busy {
            self.status = "Policy can be changed after the active turn".into();
            return;
        }
        self.policy = match self.policy {
            AccessMode::ReadOnly => AccessMode::WorkspaceWrite,
            AccessMode::WorkspaceWrite => AccessMode::FullAccess,
            AccessMode::FullAccess => AccessMode::ReadOnly,
        };
        self.status = format!("Policy: {}", self.policy.label());
    }

    fn enter_plan_mode(&mut self) {
        if self.plan_mode {
            self.status = "Already in plan mode".into();
            return;
        }
        self.previous_policy = Some(self.policy);
        self.policy = AccessMode::ReadOnly;
        self.plan_mode = true;
        self.status = "Plan mode entered — read-only. Use /exit_plan to leave.".into();
        self.push_notice(
            NoticeKind::Success,
            "Plan mode entered. Policy is now read-only. Use /exit_plan to return.",
        );
    }

    fn exit_plan_mode(&mut self) {
        if !self.plan_mode {
            self.status = "Not in plan mode".into();
            return;
        }
        if let Some(previous) = self.previous_policy.take() {
            self.policy = previous;
        } else {
            self.policy = AccessMode::WorkspaceWrite;
        }
        self.plan_mode = false;
        self.status = format!("Plan mode exited — policy: {}", self.policy.label());
        self.push_notice(
            NoticeKind::Success,
            format!(
                "Plan mode exited. Policy restored to {}.",
                self.policy.label()
            ),
        );
    }
}

fn open_settings(app: &mut TuiState, runtime_tx: mpsc::UnboundedSender<RuntimeEvent>) {
    app.open_overlay(OverlayKind::Settings);
    if app.overlay.as_ref().map(|overlay| overlay.kind) == Some(OverlayKind::Settings) {
        start_mcp_status_refresh(app, runtime_tx);
    }
}

fn refresh_settings_state(app: &mut TuiState, refresh: SettingsRefresh) {
    let result = match refresh {
        SettingsRefresh::Mcp => list_configured_mcp(&app.home).map(|items| (Some(items), None)),
        SettingsRefresh::Skills => {
            list_installed_skills(&app.home).map(|items| (None, Some(items)))
        }
    };
    match result {
        Ok((mcp, skills)) => {
            if let Some(settings) = app.settings.as_mut() {
                if let Some(mcp) = mcp {
                    settings.mcp_servers = mcp;
                    settings.mcp_statuses.clear();
                }
                if let Some(skills) = skills {
                    settings.skills = skills;
                }
            }
            let count = settings_item_count(app);
            if let Some(settings) = app.settings.as_mut() {
                settings.selected = settings.selected.min(count.saturating_sub(1));
            }
        }
        Err(error) => app.push_notice(NoticeKind::Error, error.to_string()),
    }
}

enum RuntimeEvent {
    Agent {
        generation: u64,
        event: AgentEvent,
    },
    Approval {
        generation: u64,
        call: ToolCall,
        reason: String,
        responder: oneshot::Sender<bool>,
    },
    UserInput {
        generation: u64,
        request: UserInputRequest,
        responder: oneshot::Sender<Option<UserInputResponse>>,
    },
    UserInputExpired {
        generation: u64,
    },
    TurnFinished {
        generation: u64,
        session: Session,
        error: Option<String>,
    },
    CatalogInstalled {
        id: String,
        result: std::result::Result<PathBuf, String>,
    },
    AutoConfigFinished(Option<std::result::Result<AutoConfigResult, String>>),
    UpdateChecked(std::result::Result<UpdateCheckResult, String>),
    McpStatuses(Vec<McpServerStatus>),
    SettingsActionFinished {
        result: std::result::Result<String, String>,
        refresh: SettingsRefresh,
    },
    SideAgent(AgentEvent),
    SideFinished(Option<String>),
}

#[derive(Clone, Copy)]
enum SettingsRefresh {
    Mcp,
    Skills,
}

fn start_update_check(runtime_tx: mpsc::UnboundedSender<RuntimeEvent>) {
    tokio::spawn(async move {
        let result = check_for_update(env!("CARGO_PKG_VERSION"))
            .await
            .map_err(|error| format!("Update check failed: {error:#}"));
        let _ = runtime_tx.send(RuntimeEvent::UpdateChecked(result));
    });
}

fn start_mcp_status_refresh(app: &mut TuiState, runtime_tx: mpsc::UnboundedSender<RuntimeEvent>) {
    if app.settings_busy {
        return;
    }
    app.settings_busy = true;
    app.status = "Checking MCP servers".into();
    let home = app.home.clone();
    tokio::spawn(async move {
        let runtime = McpRuntime::load(&home).await;
        let _ = runtime_tx.send(RuntimeEvent::McpStatuses(runtime.statuses().to_vec()));
    });
}

struct ChannelObserver {
    generation: u64,
    sender: mpsc::UnboundedSender<RuntimeEvent>,
}

struct SideObserver {
    sender: mpsc::UnboundedSender<RuntimeEvent>,
}

impl AgentObserver for SideObserver {
    fn on_event(&self, event: &AgentEvent) {
        let _ = self.sender.send(RuntimeEvent::SideAgent(event.clone()));
    }
}

struct SideApproval;

#[async_trait]
impl ApprovalHandler for SideApproval {
    async fn approve(&self, _call: &ToolCall, _reason: &str) -> bool {
        false
    }
}

impl AgentObserver for ChannelObserver {
    fn on_event(&self, event: &AgentEvent) {
        let _ = self.sender.send(RuntimeEvent::Agent {
            generation: self.generation,
            event: event.clone(),
        });
    }
}

struct ChannelApproval {
    generation: u64,
    approve_all: bool,
    sender: mpsc::UnboundedSender<RuntimeEvent>,
}

#[async_trait]
impl ApprovalHandler for ChannelApproval {
    async fn approve(&self, call: &ToolCall, reason: &str) -> bool {
        if self.approve_all {
            return true;
        }
        let (responder, receiver) = oneshot::channel();
        if self
            .sender
            .send(RuntimeEvent::Approval {
                generation: self.generation,
                call: call.clone(),
                reason: reason.to_string(),
                responder,
            })
            .is_err()
        {
            return false;
        }
        receiver.await.unwrap_or(false)
    }

    async fn request_user_input(&self, request: &UserInputRequest) -> Option<UserInputResponse> {
        let (responder, receiver) = oneshot::channel();
        if self
            .sender
            .send(RuntimeEvent::UserInput {
                generation: self.generation,
                request: request.clone(),
                responder,
            })
            .is_err()
        {
            return None;
        }
        if let Some(timeout_ms) = request.auto_resolution_ms {
            match tokio::time::timeout(Duration::from_millis(timeout_ms), receiver).await {
                Ok(response) => response.ok().flatten(),
                Err(_) => {
                    let _ = self.sender.send(RuntimeEvent::UserInputExpired {
                        generation: self.generation,
                    });
                    Some(
                        request
                            .questions
                            .iter()
                            .filter_map(|question| {
                                question
                                    .options
                                    .first()
                                    .map(|option| (question.id.clone(), option.label.clone()))
                            })
                            .collect(),
                    )
                }
            }
        } else {
            receiver.await.ok().flatten()
        }
    }
}

fn start_agent_turn(
    app: &mut TuiState,
    prompt: String,
    loop_continuation: bool,
    runtime_tx: mpsc::UnboundedSender<RuntimeEvent>,
) {
    let registry = match load_registry(&app.home) {
        Ok(registry) => registry,
        Err(error) => {
            app.push_notice(NoticeKind::Error, error.to_string());
            return;
        }
    };
    let provider_config = match provider_for_session(&registry, &app.session, None) {
        Ok(provider) => provider,
        Err(error) => {
            app.push_notice(NoticeKind::Error, error.to_string());
            return;
        }
    };
    let instructions = match coomi_engine::discover_project_instructions(&app.cwd) {
        Ok(instructions) => instructions,
        Err(error) => {
            app.push_notice(NoticeKind::Error, error.to_string());
            return;
        }
    };

    if !loop_continuation {
        app.input_history.push(prompt.clone());
        app.history_cursor = None;
        app.timeline.push(TimelineEntry::User(prompt.clone()));
    }
    app.follow_tail = true;
    app.busy = true;
    app.status = if loop_continuation {
        format!("Continuing Loop with {}", provider_config.model)
    } else {
        format!("Working with {}", provider_config.model)
    };
    let generation = app.next_generation;
    app.next_generation = app.next_generation.saturating_add(1);
    app.active_generation = Some(generation);

    let mut session = app.session.clone();
    let cwd = app.cwd.clone();
    let home = app.home.clone();
    let policy = app.policy;
    let approve_all = app.approve_all;
    let prompt_for_task = prompt;
    let system_prompt = system_prompt(&cwd, policy, &instructions, &home);
    let task_tx = runtime_tx.clone();
    let input_queue = Arc::clone(&app.input_queue);
    let task = tokio::spawn(async move {
        let result = async {
            let security = SecurityPolicy::new(&cwd, policy)?;
            let scheduler = AgentScheduler::new(
                cwd.clone(),
                home.clone(),
                provider_config.clone(),
                policy,
                system_prompt.clone(),
            );
            let tools = CoreTools::new(cwd.clone(), security)
                .with_skills_directory(home.join("skills"))
                .with_config_home(home.clone())
                .with_session_state(session.plan.clone(), session.loop_state.clone())
                .with_mcp_runtime(Arc::new(McpRuntime::load(&home).await))
                .with_memory(Arc::new(MemoryManager::new(&home, &cwd)))
                .with_hooks(Arc::new(HookRunner::load(&home)?))
                .with_file_history(home.clone(), session.id.to_string())
                .with_agent_scheduler(scheduler, session.messages.clone());
            let provider = HttpModelProvider::new(provider_config)?;
            let observer = ChannelObserver {
                generation,
                sender: task_tx.clone(),
            };
            let approval = ChannelApproval {
                generation,
                approve_all,
                sender: task_tx.clone(),
            };
            let checkpoint_home = home.clone();
            let agent = Agent::new(system_prompt)
                .with_input_queue(input_queue)
                .with_durable_checkpoint(Arc::new(move |session| {
                    SessionStore::new(&checkpoint_home)
                        .save(session)
                        .map_err(|error| format!("{error:#}"))
                }));
            let turn = if loop_continuation {
                agent
                    .continue_loop(&mut session, &provider, &tools, &approval, &observer)
                    .await
            } else {
                agent
                    .run_turn(
                        &mut session,
                        prompt_for_task,
                        &provider,
                        &tools,
                        &approval,
                        &observer,
                    )
                    .await
            };
            turn.map(|_| ()).map_err(anyhow::Error::from)
        }
        .await;
        let _ = task_tx.send(RuntimeEvent::TurnFinished {
            generation,
            session,
            error: result.err().map(|error| format!("{error:#}")),
        });
    });
    app.active_abort = Some(task.abort_handle());
}

fn start_manual_compaction(app: &mut TuiState, runtime_tx: mpsc::UnboundedSender<RuntimeEvent>) {
    if !require_idle(app, "compact context") {
        return;
    }
    if app.session.messages.is_empty() {
        app.push_notice(
            NoticeKind::Warning,
            "The current session has no context to compact",
        );
        return;
    }
    let registry = match load_registry(&app.home) {
        Ok(registry) => registry,
        Err(error) => {
            app.push_notice(NoticeKind::Error, error.to_string());
            return;
        }
    };
    let provider_config = match provider_for_session(&registry, &app.session, None) {
        Ok(provider) => provider,
        Err(error) => {
            app.push_notice(NoticeKind::Error, error.to_string());
            return;
        }
    };
    let instructions = match coomi_engine::discover_project_instructions(&app.cwd) {
        Ok(instructions) => instructions,
        Err(error) => {
            app.push_notice(NoticeKind::Error, error.to_string());
            return;
        }
    };
    app.close_overlay();
    app.busy = true;
    app.status = "Compacting context".into();
    let generation = app.next_generation;
    app.next_generation = app.next_generation.saturating_add(1);
    app.active_generation = Some(generation);

    let mut session = app.session.clone();
    let cwd = app.cwd.clone();
    let home = app.home.clone();
    let policy = app.policy;
    let prompt = system_prompt(&cwd, policy, &instructions, &home);
    let task_tx = runtime_tx.clone();
    let task = tokio::spawn(async move {
        let result = async {
            let security = SecurityPolicy::new(&cwd, policy)?;
            let scheduler = AgentScheduler::new(
                cwd.clone(),
                home.clone(),
                provider_config.clone(),
                policy,
                prompt.clone(),
            );
            let tools = CoreTools::new(cwd.clone(), security)
                .with_skills_directory(home.join("skills"))
                .with_config_home(home.clone())
                .with_session_state(session.plan.clone(), session.loop_state.clone())
                .with_mcp_runtime(Arc::new(McpRuntime::load(&home).await))
                .with_memory(Arc::new(MemoryManager::new(&home, &cwd)))
                .with_hooks(Arc::new(HookRunner::load(&home)?))
                .with_file_history(home.clone(), session.id.to_string())
                .with_agent_scheduler(scheduler, session.messages.clone());
            let provider = HttpModelProvider::new(provider_config)?;
            Agent::new(prompt)
                .compact_session(
                    &mut session,
                    &provider,
                    &tools,
                    &ChannelObserver {
                        generation,
                        sender: task_tx.clone(),
                    },
                )
                .await
                .map_err(anyhow::Error::from)
        }
        .await;
        let _ = task_tx.send(RuntimeEvent::TurnFinished {
            generation,
            session,
            error: result.err().map(|error| format!("{error:#}")),
        });
    });
    app.active_abort = Some(task.abort_handle());
}

fn start_side_session(
    app: &mut TuiState,
    prompt: String,
    runtime_tx: mpsc::UnboundedSender<RuntimeEvent>,
) {
    if app.side_busy || prompt.trim().is_empty() {
        return;
    }
    let registry = match load_registry(&app.home) {
        Ok(registry) => registry,
        Err(error) => {
            app.push_notice(NoticeKind::Error, error.to_string());
            return;
        }
    };
    let provider_config = match provider_for_session(&registry, &app.session, None) {
        Ok(provider) => provider,
        Err(error) => {
            app.push_notice(NoticeKind::Error, error.to_string());
            return;
        }
    };
    app.side_busy = true;
    app.timeline.push(TimelineEntry::SideUser(prompt.clone()));
    app.follow_tail = true;
    let mut session = app.session.clone();
    session.id = Uuid::new_v4();
    session.loop_state = None;
    let cwd = app.cwd.clone();
    let home = app.home.clone();
    let sender = runtime_tx.clone();
    tokio::spawn(async move {
        let result = async {
            let provider = HttpModelProvider::new(provider_config)?;
            let tools = CoreTools::new(
                cwd.clone(),
                SecurityPolicy::new(&cwd, AccessMode::ReadOnly)?,
            )
            .with_skills_directory(home.join("skills"))
            .with_config_home(home.clone());
            let instructions = coomi_engine::discover_project_instructions(&cwd)?;
            let prompt_base = system_prompt(&cwd, AccessMode::ReadOnly, &instructions, &home);
            let side_prompt = format!(
                "{prompt_base}\n\nThis is a temporary Side Session. It is read-only, must not mutate files or persistent state, and must not claim that deferred changes were applied. Answer from the cloned context and keep the main task independent."
            );
            Agent::new(side_prompt)
                .run_turn(
                    &mut session,
                    prompt,
                    &provider,
                    &tools,
                    &SideApproval,
                    &SideObserver {
                        sender: sender.clone(),
                    },
                )
                .await
                .map(|_| ())
                .map_err(anyhow::Error::from)
        }
        .await;
        let _ = sender.send(RuntimeEvent::SideFinished(
            result.err().map(|error| format!("{error:#}")),
        ));
    });
}

fn handle_runtime_event(app: &mut TuiState, runtime_event: RuntimeEvent) {
    match runtime_event {
        RuntimeEvent::CatalogInstalled { id, result } => {
            app.catalog_busy = None;
            match result {
                Ok(path) => {
                    refresh_settings_state(app, SettingsRefresh::Skills);
                    app.push_notice(
                        NoticeKind::Success,
                        format!("Installed {id} at {}", path.display()),
                    );
                }
                Err(error) => app.push_notice(NoticeKind::Error, error),
            }
        }
        RuntimeEvent::AutoConfigFinished(result) => {
            app.auto_config_busy = false;
            match result {
                None => {
                    app.status = "Configuration cancelled".into();
                    app.push_notice(NoticeKind::Warning, "Auto configuration was not applied");
                }
                Some(Err(error)) => {
                    app.status = "Configuration failed".into();
                    app.push_notice(NoticeKind::Error, error);
                }
                Some(Ok(result)) => {
                    app.status = format!("{} ready", result.kind);
                    app.push_notice(NoticeKind::Success, result.message);
                    if result.kind == "provider" {
                        refresh_active_provider(app);
                    }
                }
            }
        }
        RuntimeEvent::UpdateChecked(result) => match result {
            Ok(result) if result.update_available => {
                app.update_status = format!(
                    "Update available: {} -> {}",
                    result.current_version, result.latest_version
                );
                app.pending_update = Some(PendingUpdate {
                    current_version: result.current_version,
                    latest_version: result.latest_version,
                    release_url: result.release_url,
                    selected: UpdateChoice::No,
                });
            }
            Ok(result) => {
                app.update_status = format!("Coomi {} is current", result.current_version);
            }
            Err(error) => app.update_status = error,
        },
        RuntimeEvent::McpStatuses(statuses) => {
            app.settings_busy = false;
            if let Some(settings) = app.settings.as_mut() {
                settings.mcp_statuses = statuses;
            }
            app.status = "MCP status refreshed".into();
        }
        RuntimeEvent::SettingsActionFinished { result, refresh } => {
            app.settings_busy = false;
            refresh_settings_state(app, refresh);
            match result {
                Ok(message) => {
                    app.status = message.clone();
                    app.push_notice(NoticeKind::Success, message);
                }
                Err(error) => {
                    app.status = "Settings action failed".into();
                    app.push_notice(NoticeKind::Error, error);
                }
            }
        }
        RuntimeEvent::Approval {
            generation,
            call,
            reason,
            responder,
        } if app.active_generation == Some(generation) => {
            app.pending_approval = Some(PendingApproval {
                call,
                reason,
                selected: ApprovalChoice::Approve,
                responder: Some(responder),
            });
        }
        RuntimeEvent::UserInput {
            generation,
            request,
            responder,
        } if app.active_generation == Some(generation) => {
            app.pending_user_input = Some(PendingUserInput {
                request,
                question_index: 0,
                option_index: 0,
                other_editor: None,
                answers: UserInputResponse::new(),
                responder: Some(responder),
            });
            app.status = "Waiting for your answer".into();
        }
        RuntimeEvent::UserInputExpired { generation }
            if app.active_generation == Some(generation) =>
        {
            app.pending_user_input = None;
            app.status = "Question auto-resolved".into();
        }
        RuntimeEvent::SideAgent(event) => match event {
            AgentEvent::Text(text) | AgentEvent::TextDelta(text) if !text.is_empty() => {
                match app.timeline.last_mut() {
                    Some(TimelineEntry::SideAssistant(current)) => current.push_str(&text),
                    _ => app.timeline.push(TimelineEntry::SideAssistant(text)),
                }
            }
            AgentEvent::ReasoningDelta(_) => app.status = "Side Session thinking".into(),
            AgentEvent::ToolStarted(call) => {
                app.status = format!("Side Session reading with {}", call.name)
            }
            _ => {}
        },
        RuntimeEvent::SideFinished(error) => {
            app.side_busy = false;
            if let Some(error) = error {
                app.push_notice(NoticeKind::Error, format!("Side Session: {error}"));
            } else if app.busy {
                app.status = "Main task still running".into();
            }
        }
        RuntimeEvent::Agent { generation, event } if app.active_generation == Some(generation) => {
            match event {
                AgentEvent::ModelStarted { round, .. } => {
                    app.status = if round == 1 {
                        "Thinking".into()
                    } else {
                        format!("Continuing tool loop, round {round}")
                    };
                }
                AgentEvent::Text(text) => {
                    if !text.is_empty() {
                        app.timeline.push(TimelineEntry::Assistant(text));
                    }
                }
                AgentEvent::TextDelta(text) => {
                    if !text.is_empty() {
                        match app.timeline.last_mut() {
                            Some(TimelineEntry::Assistant(current)) => current.push_str(&text),
                            _ => app.timeline.push(TimelineEntry::Assistant(text)),
                        }
                    }
                }
                AgentEvent::ReasoningDelta(text) => {
                    if !text.is_empty() {
                        match app.timeline.last_mut() {
                            Some(TimelineEntry::Reasoning(current)) => current.push_str(&text),
                            _ => app.timeline.push(TimelineEntry::Reasoning(text)),
                        }
                    }
                }
                AgentEvent::ContextUpdated(status) => app.context_status = status,
                AgentEvent::CompactionStarted { automatic } => {
                    app.status = if automatic {
                        "Compacting context".into()
                    } else {
                        "Compacting context (manual)".into()
                    };
                }
                AgentEvent::CompactionCompleted {
                    before_tokens,
                    after_tokens,
                    ..
                } => app.push_notice(
                    NoticeKind::Success,
                    format!(
                        "Context compacted: {} -> {} tokens",
                        before_tokens, after_tokens
                    ),
                ),
                AgentEvent::PlanUpdated(plan) => {
                    let complete = plan
                        .steps
                        .iter()
                        .filter(|step| step.status == coomi_engine::PlanStepStatus::Completed)
                        .count();
                    app.status = format!("Plan {complete}/{}", plan.steps.len());
                }
                AgentEvent::LoopUpdated(loop_state) => {
                    app.status = format!("Loop: {:?}", loop_state.status).to_ascii_lowercase();
                }
                AgentEvent::QueuedInputAccepted(messages) => {
                    for message in messages {
                        if app.queue.front().is_some_and(|queued| queued == &message) {
                            app.queue.pop_front();
                        }
                        app.timeline.push(TimelineEntry::User(message));
                    }
                    app.status = "Queued input added to the active turn".into();
                }
                AgentEvent::ToolStarted(call) => {
                    app.status = format!("Running {}", call.name);
                    app.timeline.push(TimelineEntry::Tool {
                        id: call.id,
                        name: call.name,
                        arguments: call.arguments,
                        state: ToolState::Running,
                    });
                }
                AgentEvent::ToolFinished { call, result } => {
                    if let Some(TimelineEntry::Tool { state, .. }) = app.timeline.iter_mut().rev().find(
                        |entry| matches!(entry, TimelineEntry::Tool { id, .. } if id == &call.id),
                    ) {
                        *state = ToolState::Complete {
                            success: result.success,
                            output: result.output,
                        };
                    }
                }
                AgentEvent::ConnectionRetry {
                    attempt,
                    max_attempts,
                    delay_ms,
                    message,
                } => {
                    app.push_notice(
                        NoticeKind::Warning,
                        format!(
                            "Reconnecting {attempt}/{max_attempts} in {delay_ms} ms: {message}"
                        ),
                    );
                    app.status = "Reconnecting".into();
                }
                AgentEvent::StreamReset => app.status = "Resetting partial response".into(),
                AgentEvent::ReasoningCompleted => {}
                AgentEvent::PersistenceFailed(error) => app.push_notice(
                    NoticeKind::Error,
                    format!("Session persistence failed: {error}"),
                ),
                AgentEvent::ModelUsage { .. } => {}
                AgentEvent::TurnCompleted { .. } => app.status = "Finalizing".into(),
            }
        }
        RuntimeEvent::TurnFinished {
            generation,
            session,
            error,
        } if app.active_generation == Some(generation) => {
            app.busy = false;
            app.active_generation = None;
            app.active_abort = None;
            app.pending_approval = None;
            app.pending_user_input = None;
            app.session = session;
            match SessionStore::new(&app.home).save(&app.session) {
                Ok(()) => {}
                Err(save_error) => {
                    app.push_notice(NoticeKind::Error, save_error.to_string());
                }
            }
            if let Some(error) = error {
                app.status = "Turn failed".into();
                app.push_notice(NoticeKind::Error, error);
            } else {
                app.status = "Ready".into();
                app.loop_continuation_pending = app
                    .session
                    .loop_state
                    .as_ref()
                    .is_some_and(|state| state.status == coomi_engine::LoopStatus::Active);
            }
            app.refresh_sessions();
        }
        _ => {}
    }
}

fn handle_mouse(app: &mut TuiState, mouse: MouseEvent) {
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            app.scroll_up(3);
        }
        MouseEventKind::ScrollDown => {
            app.scroll_down(3);
        }
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(area) = app.scrollbar_area.get() {
                if mouse.column >= area.x
                    && mouse.column < area.x + area.width
                    && mouse.row >= area.y
                    && mouse.row < area.y + area.height
                {
                    app.scrollbar_dragging = true;
                    scroll_to_mouse(app, area, mouse.row);
                }
            }
        }
        MouseEventKind::Drag(MouseButton::Left) if app.scrollbar_dragging => {
            if let Some(area) = app.scrollbar_area.get() {
                scroll_to_mouse(app, area, mouse.row);
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            app.scrollbar_dragging = false;
        }
        _ => {}
    }
}

fn scroll_to_mouse(app: &mut TuiState, area: Rect, row: u16) {
    let max_scroll = app.timeline_max_scroll.get();
    if max_scroll == 0 || area.height <= 1 {
        return;
    }
    let relative_row = row.saturating_sub(area.y);
    let ratio = relative_row as f64 / (area.height.saturating_sub(1)) as f64;
    let target = (ratio * max_scroll as f64).round() as u16;
    app.scroll = target.min(max_scroll);
    app.follow_tail = app.scroll >= max_scroll;
}

fn handle_key(
    app: &mut TuiState,
    key: KeyEvent,
    runtime_tx: mpsc::UnboundedSender<RuntimeEvent>,
) -> Result<()> {
    if app.pending_update.is_some() {
        return handle_update_key(app, key);
    }
    if app.plan_mode_confirm.is_some() {
        return handle_plan_mode_confirm_key(app, key, runtime_tx);
    }
    if app.pending_approval.is_some() {
        return handle_approval_key(app, key);
    }
    if app.pending_user_input.is_some() {
        return handle_user_input_key(app, key);
    }
    if app.confirm_delete.is_some() {
        return handle_delete_confirmation_key(app, key);
    }
    if app.overlay.is_some() {
        return handle_overlay_key(app, key, runtime_tx);
    }

    if key.modifiers.contains(KeyModifiers::ALT) {
        match key.code {
            KeyCode::Char('i' | 'I') | KeyCode::Enter => {
                let prompt = app.editor.take().trim().to_owned();
                start_side_session(app, prompt, runtime_tx);
            }
            KeyCode::Char('m') => app.open_overlay(OverlayKind::Models),
            KeyCode::Char('s') if require_idle(app, "open Settings") => {
                open_settings(app, runtime_tx)
            }
            KeyCode::Char('h') => app.open_overlay(OverlayKind::Help),
            KeyCode::Char('l') => {
                if app.session.loop_state.is_some() {
                    show_loop_status(app);
                } else {
                    app.editor.set("/loop ");
                }
            }
            KeyCode::Up => recall_input(app, -1),
            KeyCode::Down => recall_input(app, 1),
            _ => {}
        }
        return Ok(());
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('k') => app.open_overlay(OverlayKind::Commands),
            KeyCode::Char('r') => app.open_overlay(OverlayKind::History),
            KeyCode::Char('l') => {
                app.timeline.clear();
                app.status = "Timeline cleared".into();
            }
            KeyCode::Char('j') => app.editor.insert('\n'),
            KeyCode::Char('c') => {
                if app.busy {
                    cancel_active_turn(app);
                } else if app.editor.is_empty() {
                    app.quit = true;
                } else {
                    app.editor.clear();
                }
            }
            KeyCode::Char('y') => {
                let content = app
                    .timeline
                    .iter()
                    .rev()
                    .find_map(|entry| match entry {
                        TimelineEntry::Assistant(text) => Some(text.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                if content.is_empty() {
                    app.push_notice(NoticeKind::Warning, "No assistant message to copy");
                } else if crate::clipboard::copy_best(&content) {
                    app.status = "Last response copied to clipboard".into();
                    app.push_notice(
                        NoticeKind::Success,
                        "Copied last assistant response to clipboard",
                    );
                } else {
                    app.push_notice(
                        NoticeKind::Error,
                        "Failed to copy to clipboard (clipboard not available)",
                    );
                }
            }
            _ => {}
        }
        return Ok(());
    }

    match key.code {
        KeyCode::BackTab => app.cycle_policy(),
        KeyCode::Esc => handle_escape(app),
        KeyCode::PageUp => app.scroll_up(8),
        KeyCode::PageDown => app.scroll_down(8),
        KeyCode::Home if app.editor.is_empty() => {
            app.scroll = 0;
            app.follow_tail = false;
        }
        KeyCode::End if app.editor.is_empty() => {
            app.scroll = 0;
            app.follow_tail = true;
        }
        KeyCode::Up if app.editor.is_empty() => recall_input(app, -1),
        KeyCode::Down if app.editor.is_empty() => recall_input(app, 1),
        KeyCode::Up => app.editor.move_up(),
        KeyCode::Down => app.editor.move_down(),
        KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => app.editor.insert('\n'),
        KeyCode::Enter => submit_editor(app, runtime_tx)?,
        KeyCode::Backspace => app.editor.backspace(),
        KeyCode::Delete => app.editor.delete(),
        KeyCode::Left => app.editor.move_left(),
        KeyCode::Right => app.editor.move_right(),
        KeyCode::Home => app.editor.move_home(),
        KeyCode::End => app.editor.move_end(),
        KeyCode::Tab => app.editor.insert_str("    "),
        KeyCode::Char('/') if app.editor.is_empty() => {
            app.editor.insert('/');
            app.open_overlay(OverlayKind::Commands);
        }
        KeyCode::Char(character) => app.editor.insert(character),
        _ => {}
    }
    Ok(())
}

fn submit_editor(
    app: &mut TuiState,
    runtime_tx: mpsc::UnboundedSender<RuntimeEvent>,
) -> Result<()> {
    let prompt = app.editor.take();
    let prompt = prompt.trim().to_string();
    if prompt.is_empty() {
        return Ok(());
    }
    if let Some(intent) = detect_auto_config(&prompt) {
        if app.busy || app.auto_config_busy {
            app.editor.set(&prompt);
            app.push_notice(
                NoticeKind::Warning,
                "Apply pasted configuration after the active operation finishes",
            );
            return Ok(());
        }
        start_auto_config(app, intent, runtime_tx);
        return Ok(());
    }
    if prompt.starts_with('/') {
        return execute_slash_command(app, &prompt, runtime_tx);
    }
    if app.plan_mode {
        app.plan_mode_confirm = Some(PlanModeConfirm {
            prompt,
            selected: PlanModeChoice::ExitAndSend,
        });
        return Ok(());
    }
    if app.busy {
        app.input_queue.push(prompt.clone());
        app.queue.push_back(prompt);
        app.status = format!("{} queued message(s)", app.queue.len());
    } else {
        start_agent_turn(app, prompt, false, runtime_tx);
    }
    Ok(())
}

fn start_auto_config(
    app: &mut TuiState,
    intent: AutoConfigIntent,
    runtime_tx: mpsc::UnboundedSender<RuntimeEvent>,
) {
    let (call, reason) = auto_config_approval(&intent);
    let home = app.home.clone();
    let (responder, receiver) = oneshot::channel();
    app.auto_config_busy = true;
    app.status = "Configuration approval required".into();
    if app.approve_all {
        let _ = responder.send(true);
    } else {
        app.pending_approval = Some(PendingApproval {
            call,
            reason,
            selected: ApprovalChoice::Approve,
            responder: Some(responder),
        });
    }
    tokio::spawn(async move {
        let approved = receiver.await.unwrap_or(false);
        let result = if approved {
            Some(
                apply_auto_config(&home, intent)
                    .await
                    .map_err(|error| format!("Auto configuration failed: {error:#}")),
            )
        } else {
            None
        };
        let _ = runtime_tx.send(RuntimeEvent::AutoConfigFinished(result));
    });
}

fn auto_config_approval(intent: &AutoConfigIntent) -> (ToolCall, String) {
    let (name, arguments, subject) = match intent {
        AutoConfigIntent::Provider(value) => {
            let object = value.as_object();
            let provider = object
                .and_then(|object| object.get("id").or_else(|| object.get("provider")))
                .and_then(Value::as_str)
                .unwrap_or("pasted provider");
            let model = object
                .and_then(|object| object.get("model"))
                .and_then(Value::as_str)
                .unwrap_or("configured model");
            (
                "configure_provider",
                serde_json::json!({"provider": provider, "model": model, "api_key": "hidden"}),
                "Provider configuration",
            )
        }
        AutoConfigIntent::Mcp(value) => {
            let servers = value
                .get("servers")
                .or_else(|| value.get("mcpServers"))
                .and_then(Value::as_object)
                .map(|servers| servers.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_else(|| vec!["pasted server".into()]);
            (
                "configure_mcp",
                serde_json::json!({"servers": servers}),
                "MCP configuration",
            )
        }
        AutoConfigIntent::McpCommand(command) => (
            "configure_mcp",
            serde_json::json!({"command": command}),
            "MCP command",
        ),
        AutoConfigIntent::Skill(source) => (
            "install_skill",
            serde_json::json!({"source": source}),
            "Skill installation",
        ),
    };
    (
        ToolCall {
            id: format!("auto-config-{}", Uuid::new_v4()),
            name: name.into(),
            arguments,
        },
        format!("Allow Coomi to apply the detected {subject}?"),
    )
}

fn refresh_active_provider(app: &mut TuiState) {
    let result = load_registry(&app.home).and_then(|registry| {
        let provider = registry.resolve(None)?;
        app.models = registry.choices();
        app.session.switch_model(&provider.id, &provider.model);
        SessionStore::new(&app.home).save(&app.session)?;
        Ok(provider)
    });
    match result {
        Ok(provider) => app.status = format!("Provider: {} / {}", provider.display, provider.model),
        Err(error) => app.push_notice(
            NoticeKind::Warning,
            format!("Configuration saved, but provider refresh failed: {error:#}"),
        ),
    }
}

fn execute_slash_command(
    app: &mut TuiState,
    command: &str,
    runtime_tx: mpsc::UnboundedSender<RuntimeEvent>,
) -> Result<()> {
    let (name, argument) = command.split_once(' ').unwrap_or((command, ""));
    match name {
        "/model" | "/models" if argument.is_empty() => app.open_overlay(OverlayKind::Models),
        "/model" => {
            if !require_idle(app, "switch models") {
                return Ok(());
            }
            let registry = load_registry(&app.home)?;
            let provider = registry.resolve(Some(argument))?;
            app.session.switch_model(&provider.id, &provider.model);
            SessionStore::new(&app.home).save(&app.session)?;
            app.status = format!("Model: {} / {}", provider.display, provider.model);
        }
        "/history" | "/sessions" => app.open_overlay(OverlayKind::History),
        "/status" => show_session_status(app),
        "/compact" => start_manual_compaction(app, runtime_tx.clone()),
        "/mcp" => open_settings_tab(app, SettingsTab::Mcp, runtime_tx.clone()),
        "/skill" | "/skills" => open_settings_tab(app, SettingsTab::Skills, runtime_tx.clone()),
        "/memory" => handle_memory_command(app, argument)?,
        "/plan" => {
            if app.plan_mode {
                show_plan_status(app);
            } else {
                app.enter_plan_mode();
            }
        }
        "/exit_plan" => app.exit_plan_mode(),
        "/loop" if argument.is_empty() => show_loop_status(app),
        "/loop" if argument.eq_ignore_ascii_case("pause") => {
            set_loop_status(app, coomi_engine::LoopStatus::Paused)?
        }
        "/loop" if argument.eq_ignore_ascii_case("resume") => {
            set_loop_status(app, coomi_engine::LoopStatus::Active)?
        }
        "/loop" if argument.eq_ignore_ascii_case("clear") => clear_loop(app)?,
        "/loop" if argument.eq_ignore_ascii_case("edit") => edit_loop(app),
        "/loop" => create_loop(app, argument)?,
        "/new" => new_session(app)?,
        "/catalog" => app.open_overlay(OverlayKind::Catalog),
        "/settings" => open_settings(app, runtime_tx.clone()),
        "/help" => app.open_overlay(OverlayKind::Help),
        "/clear" => app.timeline.clear(),
        "/quit" | "/exit" => app.quit = true,
        _ => {
            app.push_notice(
                NoticeKind::Warning,
                format!("Unknown command: {name}. Press Ctrl+K for commands."),
            );
        }
    }
    Ok(())
}

fn open_settings_tab(
    app: &mut TuiState,
    tab: SettingsTab,
    runtime_tx: mpsc::UnboundedSender<RuntimeEvent>,
) {
    if !require_idle(app, "open Settings") {
        return;
    }
    open_settings(app, runtime_tx);
    if let Some(settings) = app.settings.as_mut() {
        settings.tab = tab;
        settings.selected = 0;
    }
}

fn show_session_status(app: &mut TuiState) {
    let context = load_registry(&app.home)
        .and_then(|registry| provider_for_session(&registry, &app.session, None))
        .map(|provider| app.session.context.status(&provider.capabilities))
        .unwrap_or_else(|_| app.context_status.clone());
    let plan = app.session.plan.as_ref().map_or_else(
        || "none".to_owned(),
        |plan| format!("{} step(s)", plan.steps.len()),
    );
    let loop_status = app.session.loop_state.as_ref().map_or_else(
        || "none".to_owned(),
        |state| format!("{:?}, {} turn(s)", state.status, state.turns_completed),
    );
    app.push_notice(
        NoticeKind::Success,
        format!(
            "{} / {} | {} | context {}% ({}/{}) | plan {} | loop {}",
            app.session.provider_id,
            app.session.model,
            app.policy.label(),
            context.used_percent,
            context.used_tokens,
            context.effective_context_window,
            plan,
            loop_status
        ),
    );
}

fn show_plan_status(app: &mut TuiState) {
    let Some(plan) = &app.session.plan else {
        app.push_notice(NoticeKind::Warning, "No Plan is configured");
        return;
    };
    let completed = plan
        .steps
        .iter()
        .filter(|step| step.status == coomi_engine::PlanStepStatus::Completed)
        .count();
    let current = plan
        .steps
        .iter()
        .find(|step| step.status == coomi_engine::PlanStepStatus::InProgress)
        .map_or("No active step", |step| step.step.as_str());
    app.push_notice(
        NoticeKind::Success,
        format!("Plan {completed}/{} | {current}", plan.steps.len()),
    );
}

fn handle_memory_command(app: &mut TuiState, argument: &str) -> Result<()> {
    let manager = MemoryManager::new(&app.home, &app.cwd);
    let (subcommand, value) = argument.split_once(' ').unwrap_or((argument, ""));
    match subcommand.to_ascii_lowercase().as_str() {
        "" | "list" => {
            let memories = manager.list();
            if memories.is_empty() {
                app.push_notice(NoticeKind::Warning, "No persistent memories");
            } else {
                app.push_notice(
                    NoticeKind::Success,
                    format!("{} persistent memory item(s)", memories.len()),
                );
                for memory in memories {
                    app.push_notice(
                        if memory.stale {
                            NoticeKind::Warning
                        } else {
                            NoticeKind::Success
                        },
                        format!(
                            "{} [{:?}/{:?}] - {}{}",
                            memory.name,
                            memory.memory_type,
                            memory.scope.unwrap_or(MemoryScope::Project),
                            memory.description,
                            if memory.stale { " [stale]" } else { "" }
                        ),
                    );
                }
            }
        }
        "add" => {
            if value.trim().is_empty() {
                app.push_notice(NoticeKind::Warning, "Usage: /memory add <content>");
                return Ok(());
            }
            let suffix = Uuid::new_v4().simple().to_string();
            let name = format!("memory-{}", &suffix[..8]);
            let description = value.chars().take(50).collect::<String>();
            let path = manager.save(
                MemoryScope::Local,
                &name,
                &description,
                MemoryType::User,
                value.trim(),
            )?;
            app.push_notice(
                NoticeKind::Success,
                format!("Saved {name} at {}", path.display()),
            );
        }
        "delete" => {
            if value.trim().is_empty() {
                app.push_notice(NoticeKind::Warning, "Usage: /memory delete <name>");
                return Ok(());
            }
            if manager.delete(value.trim())? {
                app.push_notice(
                    NoticeKind::Success,
                    format!("Deleted memory {}", value.trim()),
                );
            } else {
                app.push_notice(
                    NoticeKind::Warning,
                    format!("Memory not found: {}", value.trim()),
                );
            }
        }
        "search" => {
            if value.trim().is_empty() {
                app.push_notice(NoticeKind::Warning, "Usage: /memory search <query>");
                return Ok(());
            }
            let memories = manager.search(value.trim(), 10);
            if memories.is_empty() {
                app.push_notice(NoticeKind::Warning, "No matching memories");
            }
            for memory in memories {
                app.push_notice(
                    NoticeKind::Success,
                    format!("{} - {}", memory.name, memory.description),
                );
            }
        }
        "show" => {
            if value.trim().is_empty() {
                app.push_notice(NoticeKind::Warning, "Usage: /memory show <name>");
                return Ok(());
            }
            if let Some(memory) = manager.get(value.trim()) {
                app.timeline.push(TimelineEntry::Assistant(format!(
                    "### Memory: {}\n\nType: `{:?}`  \nScope: `{:?}`  \nUpdated: `{}`\n\n{}",
                    memory.name,
                    memory.memory_type,
                    memory.scope.unwrap_or(MemoryScope::Project),
                    memory.updated,
                    memory.content
                )));
                app.follow_tail = true;
            } else {
                app.push_notice(
                    NoticeKind::Warning,
                    format!("Memory not found: {}", value.trim()),
                );
            }
        }
        "refresh" => {
            manager.refresh_index()?;
            app.push_notice(NoticeKind::Success, "Memory index refreshed");
        }
        _ => app.push_notice(
            NoticeKind::Warning,
            "Memory commands: /memory list|add|delete|search|show|refresh",
        ),
    }
    Ok(())
}

fn recall_input(app: &mut TuiState, direction: i32) {
    if app.input_history.is_empty() {
        return;
    }
    let current = app.history_cursor.unwrap_or(app.input_history.len());
    let next = if direction < 0 {
        current.saturating_sub(1)
    } else {
        (current + 1).min(app.input_history.len())
    };
    app.history_cursor = (next < app.input_history.len()).then_some(next);
    if let Some(index) = app.history_cursor {
        app.editor.set(&app.input_history[index]);
    } else {
        app.editor.clear();
    }
}

fn handle_escape(app: &mut TuiState) {
    if app.busy {
        cancel_active_turn(app);
        return;
    }
    if !app.editor.is_empty() {
        app.editor.clear();
        app.last_escape = None;
        return;
    }
    let now = Instant::now();
    if app
        .last_escape
        .is_some_and(|previous| now.duration_since(previous) <= DOUBLE_ESCAPE_WINDOW)
    {
        app.quit = true;
    } else {
        app.last_escape = Some(now);
        app.status = "Press Esc again to quit".into();
    }
}

fn cancel_active_turn(app: &mut TuiState) {
    if let Some(abort) = app.active_abort.take() {
        abort.abort();
    }
    if let Some(mut approval) = app.pending_approval.take()
        && let Some(responder) = approval.responder.take()
    {
        let _ = responder.send(false);
    }
    if let Some(mut pending) = app.pending_user_input.take()
        && let Some(responder) = pending.responder.take()
    {
        let _ = responder.send(None);
    }
    app.busy = false;
    app.active_generation = None;
    app.queue.clear();
    app.loop_continuation_pending = false;
    if let Some(loop_state) = app.session.loop_state.as_mut()
        && loop_state.status == coomi_engine::LoopStatus::Active
    {
        loop_state.status = coomi_engine::LoopStatus::Paused;
        let _ = SessionStore::new(&app.home).save(&app.session);
    }
    app.status = "Turn cancelled".into();
    app.push_notice(NoticeKind::Warning, "Active turn cancelled");
}

fn handle_update_key(app: &mut TuiState, key: KeyEvent) -> Result<()> {
    let Some(update) = app.pending_update.as_mut() else {
        return Ok(());
    };
    match key.code {
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
            app.pending_update = None;
            app.status = "Update dismissed".into();
        }
        KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
            update.selected = update.selected.toggle();
        }
        KeyCode::Enter => {
            let choice = update.selected;
            let release_url = update.release_url.clone();
            app.pending_update = None;
            match choice {
                UpdateChoice::Yes => {
                    app.status = "Opening release page in browser".into();
                    app.push_notice(
                        NoticeKind::Success,
                        format!("Opening release page: {release_url}"),
                    );
                    let _ = open_url_in_browser(&release_url);
                }
                UpdateChoice::No => {
                    app.status = "Update dismissed".into();
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn open_url_in_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(target_os = "windows")]
    let cmd = "start";
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let cmd = "xdg-open";

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/c", cmd, url])
            .spawn()?;
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::process::Command::new(cmd).arg(url).spawn()?;
    }
    Ok(())
}

fn handle_plan_mode_confirm_key(
    app: &mut TuiState,
    key: KeyEvent,
    runtime_tx: mpsc::UnboundedSender<RuntimeEvent>,
) -> Result<()> {
    let confirm = app.plan_mode_confirm.as_mut().expect("plan mode confirm");
    match key.code {
        KeyCode::Esc => {
            app.plan_mode_confirm = None;
            return Ok(());
        }
        KeyCode::Up | KeyCode::Left => {
            confirm.selected = confirm.selected.prev();
            return Ok(());
        }
        KeyCode::Down | KeyCode::Right => {
            confirm.selected = confirm.selected.next();
            return Ok(());
        }
        KeyCode::Enter => {}
        _ => return Ok(()),
    }
    let choice = confirm.selected;
    let prompt = confirm.prompt.clone();
    app.plan_mode_confirm = None;
    match choice {
        PlanModeChoice::ExitAndSend => {
            app.exit_plan_mode();
            if app.busy {
                app.input_queue.push(prompt.clone());
                app.queue.push_back(prompt);
                app.status = format!("{} queued message(s)", app.queue.len());
            } else {
                start_agent_turn(app, prompt, false, runtime_tx);
            }
        }
        PlanModeChoice::SendReadOnly => {
            start_side_session(app, prompt, runtime_tx);
        }
        PlanModeChoice::Cancel => {}
    }
    Ok(())
}

fn handle_approval_key(app: &mut TuiState, key: KeyEvent) -> Result<()> {
    let approved = match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => Some(true),
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Some(false),
        KeyCode::Enter => app
            .pending_approval
            .as_ref()
            .map(|pending| pending.selected == ApprovalChoice::Approve),
        KeyCode::Left
        | KeyCode::Right
        | KeyCode::Up
        | KeyCode::Down
        | KeyCode::Tab
        | KeyCode::BackTab => {
            if let Some(pending) = app.pending_approval.as_mut() {
                pending.selected = pending.selected.toggle();
            }
            None
        }
        _ => None,
    };
    if let Some(approved) = approved
        && let Some(mut pending) = app.pending_approval.take()
    {
        if let Some(responder) = pending.responder.take() {
            let _ = responder.send(approved);
        }
        app.status = if approved {
            "Approved once".into()
        } else {
            "Action denied".into()
        };
    }
    Ok(())
}

fn handle_user_input_key(app: &mut TuiState, key: KeyEvent) -> Result<()> {
    let Some(pending) = app.pending_user_input.as_mut() else {
        return Ok(());
    };
    if let Some(editor) = pending.other_editor.as_mut() {
        match key.code {
            KeyCode::Esc => pending.other_editor = None,
            KeyCode::Enter => {
                let answer = editor.text().trim().to_owned();
                if !answer.is_empty() {
                    commit_user_answer(app, answer);
                }
            }
            KeyCode::Backspace => editor.backspace(),
            KeyCode::Delete => editor.delete(),
            KeyCode::Left => editor.move_left(),
            KeyCode::Right => editor.move_right(),
            KeyCode::Char(character) => editor.insert(character),
            _ => {}
        }
        return Ok(());
    }

    let options = pending
        .request
        .questions
        .get(pending.question_index)
        .map_or(1, |question| question.options.len() + 1);
    match key.code {
        KeyCode::Up | KeyCode::Left => {
            pending.option_index = pending.option_index.saturating_sub(1)
        }
        KeyCode::Down | KeyCode::Right => {
            pending.option_index = (pending.option_index + 1).min(options.saturating_sub(1));
        }
        KeyCode::Enter => {
            let answer = pending.request.questions[pending.question_index]
                .options
                .get(pending.option_index)
                .map(|option| option.label.clone());
            if let Some(answer) = answer {
                commit_user_answer(app, answer);
            } else {
                pending.other_editor = Some(Editor::default());
            }
        }
        KeyCode::Esc => {
            if let Some(mut pending) = app.pending_user_input.take()
                && let Some(responder) = pending.responder.take()
            {
                let _ = responder.send(None);
            }
            app.status = "Question cancelled".into();
        }
        _ => {}
    }
    Ok(())
}

fn commit_user_answer(app: &mut TuiState, answer: String) {
    let Some(pending) = app.pending_user_input.as_mut() else {
        return;
    };
    let question = &pending.request.questions[pending.question_index];
    pending.answers.insert(question.id.clone(), answer);
    if pending.question_index + 1 < pending.request.questions.len() {
        pending.question_index += 1;
        pending.option_index = 0;
        pending.other_editor = None;
        return;
    }
    if let Some(mut pending) = app.pending_user_input.take()
        && let Some(responder) = pending.responder.take()
    {
        let _ = responder.send(Some(pending.answers));
    }
    app.status = "Answer submitted".into();
}

fn handle_delete_confirmation_key(app: &mut TuiState, key: KeyEvent) -> Result<()> {
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
            if let Some(target) = app.confirm_delete.take() {
                match target {
                    DeleteTarget::Session(id) if id == app.session.id => {
                        app.push_notice(
                            NoticeKind::Warning,
                            "The active session cannot be deleted",
                        );
                    }
                    DeleteTarget::Session(id) => {
                        if SessionStore::new(&app.home).delete(id)? {
                            app.status = "Session deleted".into();
                            app.refresh_sessions();
                            if let Some(overlay) = &mut app.overlay {
                                overlay.selected =
                                    overlay.selected.min(app.sessions.len().saturating_sub(1));
                            }
                        }
                    }
                    DeleteTarget::Provider(id) => delete_provider(app, &id)?,
                    DeleteTarget::Mcp(name) => remove_mcp(app, &name)?,
                    DeleteTarget::Skill(name) => remove_skill(app, &name)?,
                }
            }
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
            app.confirm_delete = None;
        }
        _ => {}
    }
    Ok(())
}

fn handle_overlay_key(
    app: &mut TuiState,
    key: KeyEvent,
    runtime_tx: mpsc::UnboundedSender<RuntimeEvent>,
) -> Result<()> {
    let kind = app.overlay.as_ref().map(|overlay| overlay.kind);
    if kind == Some(OverlayKind::Help) {
        if matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
            app.close_overlay();
        }
        return Ok(());
    }
    if kind == Some(OverlayKind::Settings) {
        return handle_settings_key(app, key, runtime_tx);
    }
    if kind == Some(OverlayKind::McpConfig) {
        return handle_mcp_form_key(app, key);
    }

    match key.code {
        KeyCode::Esc => app.close_overlay(),
        KeyCode::Up => move_overlay_selection(app, -1),
        KeyCode::Down => move_overlay_selection(app, 1),
        KeyCode::PageUp => move_overlay_selection(app, -8),
        KeyCode::PageDown => move_overlay_selection(app, 8),
        KeyCode::Left if kind == Some(OverlayKind::Catalog) => {
            set_catalog_tab(app, CatalogTab::Mcp)
        }
        KeyCode::Right if kind == Some(OverlayKind::Catalog) => {
            set_catalog_tab(app, CatalogTab::Skills)
        }
        KeyCode::Tab if kind == Some(OverlayKind::Catalog) => toggle_catalog_tab(app),
        KeyCode::Backspace => {
            if let Some(overlay) = &mut app.overlay {
                overlay.query.backspace();
                overlay.selected = 0;
            }
        }
        KeyCode::Delete if kind == Some(OverlayKind::History) => {
            mark_selected_session_for_delete(app)
        }
        KeyCode::Char('D') if kind == Some(OverlayKind::History) => {
            mark_selected_session_for_delete(app)
        }
        KeyCode::Enter => activate_overlay_selection(app, runtime_tx)?,
        KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(overlay) = &mut app.overlay {
                overlay.query.insert(character);
                overlay.selected = 0;
            }
        }
        _ => {}
    }
    Ok(())
}

fn move_overlay_selection(app: &mut TuiState, direction: i32) {
    let count = overlay_item_count(app);
    if count == 0 {
        return;
    }
    if let Some(overlay) = &mut app.overlay {
        let next = (overlay.selected as i32 + direction).clamp(0, count as i32 - 1);
        overlay.selected = usize::try_from(next).unwrap_or(0);
    }
}

fn overlay_item_count(app: &TuiState) -> usize {
    let Some(overlay) = &app.overlay else {
        return 0;
    };
    let query = overlay.query.text().to_ascii_lowercase();
    match overlay.kind {
        OverlayKind::Commands => COMMANDS
            .iter()
            .filter(|item| item.label.to_ascii_lowercase().contains(&query))
            .count(),
        OverlayKind::Models => app
            .models
            .iter()
            .filter(|item| model_matches(item, &query))
            .count(),
        OverlayKind::History => app
            .sessions
            .iter()
            .filter(|item| session_matches(item, &query))
            .count(),
        OverlayKind::Catalog => match overlay.catalog_tab {
            CatalogTab::Mcp => app
                .mcp_entries
                .iter()
                .filter(|item| catalog_matches(&item.id, &item.name, &query))
                .count(),
            CatalogTab::Skills => app
                .skill_entries
                .iter()
                .filter(|item| catalog_matches(&item.id, &item.name, &query))
                .count(),
        },
        OverlayKind::Help | OverlayKind::McpConfig | OverlayKind::Settings => 0,
    }
}

fn activate_overlay_selection(
    app: &mut TuiState,
    runtime_tx: mpsc::UnboundedSender<RuntimeEvent>,
) -> Result<()> {
    let Some(overlay) = &app.overlay else {
        return Ok(());
    };
    let selected = overlay.selected;
    let query = overlay.query.text().to_ascii_lowercase();
    match overlay.kind {
        OverlayKind::Commands => {
            if let Some(item) = COMMANDS
                .iter()
                .filter(|item| item.label.to_ascii_lowercase().contains(&query))
                .nth(selected)
            {
                apply_command_action(app, item.action, runtime_tx)?;
            }
        }
        OverlayKind::Models => {
            if !require_idle(app, "switch models") {
                return Ok(());
            }
            if let Some(choice) = app
                .models
                .iter()
                .filter(|item| model_matches(item, &query))
                .nth(selected)
                .cloned()
            {
                let registry = load_registry(&app.home)?;
                let provider = registry.resolve(Some(&choice.selector))?;
                app.session.switch_model(&provider.id, &provider.model);
                SessionStore::new(&app.home).save(&app.session)?;
                app.status = format!("Model: {} / {}", provider.display, provider.model);
                app.close_overlay();
            }
        }
        OverlayKind::History => {
            if !require_idle(app, "resume another session") {
                return Ok(());
            }
            if let Some(summary) = app
                .sessions
                .iter()
                .filter(|item| session_matches(item, &query))
                .nth(selected)
                .cloned()
            {
                app.session = SessionStore::new(&app.home).load(summary.id)?;
                app.session.cwd = app.cwd.clone();
                app.rebuild_timeline();
                app.status = format!("Resumed {}", summary.id);
                app.close_overlay();
            }
        }
        OverlayKind::Catalog => activate_catalog_selection(app, selected, &query, runtime_tx)?,
        OverlayKind::Help | OverlayKind::McpConfig | OverlayKind::Settings => {}
    }
    Ok(())
}

fn apply_command_action(
    app: &mut TuiState,
    action: CommandAction,
    runtime_tx: mpsc::UnboundedSender<RuntimeEvent>,
) -> Result<()> {
    match action {
        CommandAction::NewSession => new_session(app)?,
        CommandAction::History => app.open_overlay(OverlayKind::History),
        CommandAction::Models => app.open_overlay(OverlayKind::Models),
        CommandAction::Status => {
            app.close_overlay();
            show_session_status(app);
        }
        CommandAction::Compact => start_manual_compaction(app, runtime_tx),
        CommandAction::Catalog => app.open_overlay(OverlayKind::Catalog),
        CommandAction::Mcp => open_settings_tab(app, SettingsTab::Mcp, runtime_tx),
        CommandAction::Skills => open_settings_tab(app, SettingsTab::Skills, runtime_tx),
        CommandAction::Memory => {
            app.close_overlay();
            handle_memory_command(app, "list")?;
        }
        CommandAction::Plan => {
            app.close_overlay();
            if app.plan_mode {
                show_plan_status(app);
            } else {
                app.enter_plan_mode();
            }
        }
        CommandAction::ExitPlan => {
            app.close_overlay();
            app.exit_plan_mode();
        }
        CommandAction::Loop => {
            app.editor.set("/loop ");
            app.close_overlay();
        }
        CommandAction::Settings => open_settings(app, runtime_tx),
        CommandAction::ClearTimeline => {
            app.timeline.clear();
            app.close_overlay();
        }
        CommandAction::Help => app.open_overlay(OverlayKind::Help),
        CommandAction::Quit => app.quit = true,
    }
    Ok(())
}

fn new_session(app: &mut TuiState) -> Result<()> {
    if !require_idle(app, "start a new session") {
        return Ok(());
    }
    app.session = Session::new(
        &app.session.provider_id,
        &app.session.model,
        app.cwd.clone(),
    );
    app.timeline.clear();
    app.queue.clear();
    app.status = "New session".into();
    app.close_overlay();
    SessionStore::new(&app.home).save(&app.session)?;
    app.refresh_sessions();
    Ok(())
}

fn show_loop_status(app: &mut TuiState) {
    match &app.session.loop_state {
        Some(state) => app.push_notice(
            NoticeKind::Success,
            format!(
                "Loop {:?}: {} ({} tokens, {} turns)",
                state.status, state.objective, state.tokens_used, state.turns_completed
            ),
        ),
        None => app.push_notice(NoticeKind::Warning, "No Loop is configured"),
    }
}

fn create_loop(app: &mut TuiState, objective: &str) -> Result<()> {
    if objective.trim().is_empty() {
        app.editor.set("/loop ");
        return Ok(());
    }
    if app.session.loop_state.as_ref().is_some_and(|state| {
        state.status == coomi_engine::LoopStatus::Active && state.objective != objective
    }) {
        app.push_notice(
            NoticeKind::Warning,
            "Pause or clear the active Loop before replacing it",
        );
        return Ok(());
    }
    app.session.loop_state = Some(coomi_engine::LoopState {
        objective: objective.trim().to_owned(),
        status: coomi_engine::LoopStatus::Active,
        token_budget: None,
        tokens_used: 0,
        time_used_seconds: 0,
        blocked_streak: 0,
        turns_completed: 0,
    });
    app.loop_continuation_pending = true;
    SessionStore::new(&app.home).save(&app.session)?;
    app.status = "Loop active".into();
    Ok(())
}

fn set_loop_status(app: &mut TuiState, status: coomi_engine::LoopStatus) -> Result<()> {
    let Some(loop_state) = app.session.loop_state.as_mut() else {
        app.push_notice(NoticeKind::Warning, "No Loop is configured");
        return Ok(());
    };
    loop_state.status = status;
    loop_state.blocked_streak = 0;
    app.loop_continuation_pending = status == coomi_engine::LoopStatus::Active;
    app.status = format!("Loop: {status:?}").to_ascii_lowercase();
    SessionStore::new(&app.home).save(&app.session)
}

fn clear_loop(app: &mut TuiState) -> Result<()> {
    app.session.loop_state = None;
    app.loop_continuation_pending = false;
    app.status = "Loop cleared".into();
    SessionStore::new(&app.home).save(&app.session)
}

fn edit_loop(app: &mut TuiState) {
    if let Some(loop_state) = &app.session.loop_state {
        app.editor.set(format!("/loop {}", loop_state.objective));
    } else {
        app.editor.set("/loop ");
    }
}

fn require_idle(app: &mut TuiState, action: &str) -> bool {
    if app.busy {
        app.status = format!("Cancel or finish the active turn to {action}");
        false
    } else {
        true
    }
}

fn mark_selected_session_for_delete(app: &mut TuiState) {
    let Some(overlay) = &app.overlay else {
        return;
    };
    let query = overlay.query.text().to_ascii_lowercase();
    if let Some(summary) = app
        .sessions
        .iter()
        .filter(|item| session_matches(item, &query))
        .nth(overlay.selected)
    {
        app.confirm_delete = Some(DeleteTarget::Session(summary.id));
    }
}

fn set_catalog_tab(app: &mut TuiState, tab: CatalogTab) {
    if let Some(overlay) = &mut app.overlay {
        overlay.catalog_tab = tab;
        overlay.selected = 0;
        overlay.query.clear();
    }
}

fn toggle_catalog_tab(app: &mut TuiState) {
    let tab = app.overlay.as_ref().map(|overlay| overlay.catalog_tab);
    set_catalog_tab(
        app,
        if tab == Some(CatalogTab::Mcp) {
            CatalogTab::Skills
        } else {
            CatalogTab::Mcp
        },
    );
}

fn activate_catalog_selection(
    app: &mut TuiState,
    selected: usize,
    query: &str,
    runtime_tx: mpsc::UnboundedSender<RuntimeEvent>,
) -> Result<()> {
    let tab = app
        .overlay
        .as_ref()
        .map(|overlay| overlay.catalog_tab)
        .unwrap_or(CatalogTab::Mcp);
    match tab {
        CatalogTab::Mcp => {
            if let Some(entry) = app
                .mcp_entries
                .iter()
                .filter(|item| catalog_matches(&item.id, &item.name, query))
                .nth(selected)
                .cloned()
            {
                if entry.required_parameters.is_empty() {
                    let path = CatalogInstaller::new(&app.home)
                        .install_mcp(&entry.id, &BTreeMap::new())?;
                    app.push_notice(
                        NoticeKind::Success,
                        format!("Configured {} at {}", entry.name, path.display()),
                    );
                    app.close_overlay();
                } else {
                    app.mcp_form = Some(McpForm {
                        fields: entry
                            .required_parameters
                            .iter()
                            .cloned()
                            .map(|parameter| (parameter, Editor::default()))
                            .collect(),
                        entry,
                        selected: 0,
                    });
                    app.overlay = Some(Overlay::new(OverlayKind::McpConfig));
                }
            }
        }
        CatalogTab::Skills => {
            if app.catalog_busy.is_some() {
                return Ok(());
            }
            if let Some(entry) = app
                .skill_entries
                .iter()
                .filter(|item| catalog_matches(&item.id, &item.name, query))
                .nth(selected)
                .cloned()
            {
                let id = entry.id.clone();
                let home = app.home.clone();
                app.catalog_busy = Some(id.clone());
                app.status = format!("Installing {}", entry.name);
                app.close_overlay();
                tokio::task::spawn_blocking(move || {
                    let result = CatalogInstaller::new(home)
                        .install_skill(&id)
                        .map_err(|error| format!("{error:#}"));
                    let _ = runtime_tx.send(RuntimeEvent::CatalogInstalled { id, result });
                });
            }
        }
    }
    Ok(())
}

fn handle_mcp_form_key(app: &mut TuiState, key: KeyEvent) -> Result<()> {
    let Some(form) = &mut app.mcp_form else {
        app.close_overlay();
        return Ok(());
    };
    match key.code {
        KeyCode::Esc => app.close_overlay(),
        KeyCode::Tab | KeyCode::Down => {
            form.selected = (form.selected + 1) % form.fields.len();
        }
        KeyCode::BackTab | KeyCode::Up => {
            form.selected = form
                .selected
                .checked_sub(1)
                .unwrap_or(form.fields.len() - 1);
        }
        KeyCode::Enter if form.selected + 1 < form.fields.len() => form.selected += 1,
        KeyCode::Enter => submit_mcp_form(app)?,
        KeyCode::Backspace => form.fields[form.selected].1.backspace(),
        KeyCode::Delete => form.fields[form.selected].1.delete(),
        KeyCode::Left => form.fields[form.selected].1.move_left(),
        KeyCode::Right => form.fields[form.selected].1.move_right(),
        KeyCode::Char(character) => form.fields[form.selected].1.insert(character),
        _ => {}
    }
    Ok(())
}

fn handle_settings_key(
    app: &mut TuiState,
    key: KeyEvent,
    runtime_tx: mpsc::UnboundedSender<RuntimeEvent>,
) -> Result<()> {
    let editing = app
        .settings
        .as_ref()
        .is_some_and(|settings| settings.form.is_some());
    if editing {
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('s' | 'S'))
        {
            return save_provider_form(app);
        }
        {
            let form = app
                .settings
                .as_mut()
                .and_then(|settings| settings.form.as_mut())
                .expect("provider form");
            if form.choice_open {
                match key.code {
                    KeyCode::Esc => form.choice_open = false,
                    KeyCode::Enter => {
                        if form.fields[form.selected].1.text() == CUSTOM_CONTEXT_WINDOW {
                            form.fields[form.selected].1.clear();
                        }
                        form.choice_open = false;
                    }
                    KeyCode::Up | KeyCode::Left => cycle_provider_choice(form, -1),
                    KeyCode::Down | KeyCode::Right => cycle_provider_choice(form, 1),
                    KeyCode::Home => set_provider_choice(form, 0),
                    KeyCode::End => {
                        let last = form.fields[form.selected].0.options.len().saturating_sub(1);
                        set_provider_choice(form, last);
                    }
                    KeyCode::Tab => {
                        form.choice_open = false;
                        form.selected = (form.selected + 1) % form.fields.len();
                    }
                    KeyCode::BackTab => {
                        form.choice_open = false;
                        form.selected = form
                            .selected
                            .checked_sub(1)
                            .unwrap_or(form.fields.len() - 1);
                    }
                    _ => {}
                }
                return Ok(());
            }
            match key.code {
                KeyCode::Esc => {
                    app.settings.as_mut().expect("settings").form = None;
                    return Ok(());
                }
                KeyCode::Tab | KeyCode::Down => {
                    form.selected = (form.selected + 1) % form.fields.len();
                }
                KeyCode::BackTab | KeyCode::Up => {
                    form.selected = form
                        .selected
                        .checked_sub(1)
                        .unwrap_or(form.fields.len() - 1);
                }
                KeyCode::Enter
                    if form.fields[form.selected]
                        .0
                        .is_custom_input(&form.fields[form.selected].1.text()) =>
                {
                    if form.selected + 1 < form.fields.len() {
                        form.selected += 1;
                    } else {
                        return save_provider_form(app);
                    }
                }
                KeyCode::Enter | KeyCode::Char(' ') if form.fields[form.selected].0.is_choice() => {
                    form.choice_open = true;
                }
                KeyCode::Enter if form.selected + 1 < form.fields.len() => form.selected += 1,
                KeyCode::Enter => return save_provider_form(app),
                KeyCode::Backspace => form.fields[form.selected].1.backspace(),
                KeyCode::Delete => form.fields[form.selected].1.delete(),
                KeyCode::Left if form.fields[form.selected].0.is_choice() => {
                    cycle_provider_choice(form, -1)
                }
                KeyCode::Right if form.fields[form.selected].0.is_choice() => {
                    cycle_provider_choice(form, 1)
                }
                KeyCode::Left => form.fields[form.selected].1.move_left(),
                KeyCode::Right => form.fields[form.selected].1.move_right(),
                KeyCode::Char('v' | 'V') if key.modifiers.contains(KeyModifiers::ALT) => {
                    form.show_secret = !form.show_secret;
                }
                KeyCode::Char(character)
                    if form.fields[form.selected]
                        .0
                        .is_custom_input(&form.fields[form.selected].1.text())
                        && character.is_ascii_digit() =>
                {
                    form.fields[form.selected].1.insert(character);
                }
                KeyCode::Char(character) if !form.fields[form.selected].0.is_choice() => {
                    form.fields[form.selected].1.insert(character);
                }
                _ => {}
            }
        }
        return Ok(());
    }

    let tab = app
        .settings
        .as_ref()
        .map(|settings| settings.tab)
        .unwrap_or(SettingsTab::Providers);
    let count = settings_item_count(app);
    match key.code {
        KeyCode::Esc => app.close_overlay(),
        KeyCode::Tab | KeyCode::Right => {
            if let Some(settings) = app.settings.as_mut() {
                settings.tab = settings.tab.next();
                settings.selected = 0;
                settings.error = None;
            }
        }
        KeyCode::BackTab | KeyCode::Left => {
            if let Some(settings) = app.settings.as_mut() {
                settings.tab = settings.tab.previous();
                settings.selected = 0;
                settings.error = None;
            }
        }
        KeyCode::Up => {
            if let Some(settings) = app.settings.as_mut() {
                settings.selected = settings.selected.saturating_sub(1);
            }
        }
        KeyCode::Down => {
            if let Some(settings) = app.settings.as_mut() {
                settings.selected = (settings.selected + 1).min(count.saturating_sub(1));
            }
        }
        KeyCode::Char('n' | 'N') if tab == SettingsTab::Providers => begin_provider_form(app, None),
        KeyCode::Char('e' | 'E') | KeyCode::Enter if tab == SettingsTab::Providers => {
            let selected = selected_provider_id(app);
            begin_provider_form(app, selected);
        }
        KeyCode::Char('v' | 'V') if tab == SettingsTab::Providers => {
            if let Some(settings) = app.settings.as_mut() {
                settings.show_secret = !settings.show_secret;
            }
        }
        KeyCode::Char(' ') if tab == SettingsTab::Providers => activate_selected_provider(app)?,
        KeyCode::Char('d' | 'D') | KeyCode::Delete if tab == SettingsTab::Providers => {
            request_selected_provider_delete(app)
        }
        KeyCode::Enter if tab == SettingsTab::Mcp => install_selected_mcp(app, runtime_tx.clone())?,
        KeyCode::Char(' ') if tab == SettingsTab::Mcp => toggle_selected_mcp(app)?,
        KeyCode::Char('d' | 'D') | KeyCode::Delete if tab == SettingsTab::Mcp => {
            request_selected_mcp_delete(app)
        }
        KeyCode::Char('r' | 'R') if tab == SettingsTab::Mcp => {
            start_mcp_status_refresh(app, runtime_tx)
        }
        KeyCode::Enter if tab == SettingsTab::Skills => {
            install_selected_skill(app, runtime_tx.clone())
        }
        KeyCode::Char(' ') if tab == SettingsTab::Skills => toggle_selected_skill(app)?,
        KeyCode::Char('d' | 'D') | KeyCode::Delete if tab == SettingsTab::Skills => {
            request_selected_skill_delete(app)
        }
        KeyCode::Char('u' | 'U') if tab == SettingsTab::Skills => {
            start_selected_skill_update(app, runtime_tx)
        }
        KeyCode::Char('r' | 'R') if tab == SettingsTab::Runtime => {
            app.update_status = "Checking for updates".into();
            start_update_check(runtime_tx);
        }
        KeyCode::Char('m' | 'M') if matches!(tab, SettingsTab::Mcp | SettingsTab::Skills) => {
            app.open_overlay(OverlayKind::Catalog)
        }
        _ => {}
    }
    Ok(())
}

fn settings_item_count(app: &TuiState) -> usize {
    app.settings
        .as_ref()
        .map_or(0, |settings| match settings.tab {
            SettingsTab::Providers => settings.provider_ids.len(),
            SettingsTab::Mcp => settings_mcp_items(app).len(),
            SettingsTab::Skills => settings_skill_items(app).len(),
            SettingsTab::Runtime => 0,
        })
}

fn settings_mcp_items(app: &TuiState) -> Vec<SettingsMcpItem> {
    let configured = app
        .settings
        .as_ref()
        .map(|settings| settings.mcp_servers.as_slice())
        .unwrap_or_default();
    let mut items = app
        .mcp_entries
        .iter()
        .cloned()
        .map(|entry| SettingsMcpItem {
            configured: configured
                .iter()
                .find(|item| item.name.eq_ignore_ascii_case(&entry.id))
                .cloned(),
            entry: Some(entry),
        })
        .collect::<Vec<_>>();
    items.extend(
        configured
            .iter()
            .filter(|configured| {
                !app.mcp_entries
                    .iter()
                    .any(|entry| entry.id.eq_ignore_ascii_case(&configured.name))
            })
            .cloned()
            .map(|configured| SettingsMcpItem {
                entry: None,
                configured: Some(configured),
            }),
    );
    items
}

fn settings_skill_items(app: &TuiState) -> Vec<SettingsSkillItem> {
    let installed = app
        .settings
        .as_ref()
        .map(|settings| settings.skills.as_slice())
        .unwrap_or_default();
    let mut items = app
        .skill_entries
        .iter()
        .cloned()
        .map(|entry| SettingsSkillItem {
            installed: installed
                .iter()
                .find(|item| item.name.eq_ignore_ascii_case(&entry.id))
                .cloned(),
            entry: Some(entry),
        })
        .collect::<Vec<_>>();
    items.extend(
        installed
            .iter()
            .filter(|installed| {
                !app.skill_entries
                    .iter()
                    .any(|entry| entry.id.eq_ignore_ascii_case(&installed.name))
            })
            .cloned()
            .map(|installed| SettingsSkillItem {
                entry: None,
                installed: Some(installed),
            }),
    );
    items
}

fn selected_settings_mcp(app: &TuiState) -> Option<SettingsMcpItem> {
    let selected = app.settings.as_ref()?.selected;
    settings_mcp_items(app).get(selected).cloned()
}

fn selected_settings_skill(app: &TuiState) -> Option<SettingsSkillItem> {
    let selected = app.settings.as_ref()?.selected;
    settings_skill_items(app).get(selected).cloned()
}

fn install_selected_mcp(
    app: &mut TuiState,
    runtime_tx: mpsc::UnboundedSender<RuntimeEvent>,
) -> Result<()> {
    let Some(item) = selected_settings_mcp(app) else {
        return Ok(());
    };
    if item.configured.is_some() {
        app.status = format!("MCP {} is already configured", item.id());
        return Ok(());
    }
    let Some(entry) = item.entry else {
        return Ok(());
    };
    if entry.required_parameters.is_empty() {
        CatalogInstaller::new(&app.home).install_mcp(&entry.id, &BTreeMap::new())?;
        refresh_settings_state(app, SettingsRefresh::Mcp);
        app.status = format!("Configured MCP {}", entry.name);
        start_mcp_status_refresh(app, runtime_tx);
    } else {
        app.mcp_form = Some(McpForm {
            fields: entry
                .required_parameters
                .iter()
                .cloned()
                .map(|parameter| (parameter, Editor::default()))
                .collect(),
            entry,
            selected: 0,
        });
        app.overlay = Some(Overlay::new(OverlayKind::McpConfig));
    }
    Ok(())
}

fn install_selected_skill(app: &mut TuiState, runtime_tx: mpsc::UnboundedSender<RuntimeEvent>) {
    let Some(item) = selected_settings_skill(app) else {
        return;
    };
    if item.installed.is_some() {
        app.status = format!("Skill {} is already installed", item.id());
        return;
    }
    let Some(entry) = item.entry else {
        return;
    };
    if app.catalog_busy.is_some() {
        return;
    }
    let id = entry.id.clone();
    let home = app.home.clone();
    app.catalog_busy = Some(id.clone());
    app.status = format!("Installing {}", entry.name);
    tokio::task::spawn_blocking(move || {
        let result = CatalogInstaller::new(home)
            .install_skill(&id)
            .map_err(|error| format!("{error:#}"));
        let _ = runtime_tx.send(RuntimeEvent::CatalogInstalled { id, result });
    });
}

fn toggle_selected_mcp(app: &mut TuiState) -> Result<()> {
    let Some(item) = selected_settings_mcp(app).and_then(|item| item.configured) else {
        app.status = "Install this MCP before enabling or disabling it".into();
        return Ok(());
    };
    set_mcp_enabled(&app.home, &item.name, !item.enabled)?;
    refresh_settings_state(app, SettingsRefresh::Mcp);
    app.status = format!(
        "MCP {} {}",
        item.name,
        if item.enabled { "disabled" } else { "enabled" }
    );
    Ok(())
}

fn remove_mcp(app: &mut TuiState, name: &str) -> Result<()> {
    remove_configured_mcp(&app.home, name)?;
    refresh_settings_state(app, SettingsRefresh::Mcp);
    app.status = format!("Removed MCP {name}");
    Ok(())
}

fn toggle_selected_skill(app: &mut TuiState) -> Result<()> {
    let Some(item) = selected_settings_skill(app).and_then(|item| item.installed) else {
        app.status = "Install this Skill before enabling or disabling it".into();
        return Ok(());
    };
    set_skill_enabled(&app.home, &item.name, !item.enabled)?;
    refresh_settings_state(app, SettingsRefresh::Skills);
    app.status = format!(
        "Skill {} {}",
        item.name,
        if item.enabled { "disabled" } else { "enabled" }
    );
    Ok(())
}

fn remove_skill(app: &mut TuiState, name: &str) -> Result<()> {
    remove_installed_skill(&app.home, name)?;
    refresh_settings_state(app, SettingsRefresh::Skills);
    app.status = format!("Removed Skill {name}");
    Ok(())
}

fn start_selected_skill_update(
    app: &mut TuiState,
    runtime_tx: mpsc::UnboundedSender<RuntimeEvent>,
) {
    if app.settings_busy {
        return;
    }
    let Some(item) = selected_settings_skill(app).and_then(|item| item.installed) else {
        return;
    };
    app.settings_busy = true;
    app.status = format!("Updating Skill {}", item.name);
    let home = app.home.clone();
    tokio::spawn(async move {
        let name = item.name.clone();
        let result = if item.source_type == "catalog" {
            tokio::task::spawn_blocking(move || {
                CatalogInstaller::new(home)
                    .update_skill(&name)
                    .map(|_| format!("Skill {name} updated"))
                    .map_err(|error| format!("{error:#}"))
            })
            .await
            .unwrap_or_else(|error| Err(format!("Skill update task failed: {error}")))
        } else {
            update_installed_skill(&home, &name)
                .await
                .map(|result| result.message)
                .map_err(|error| format!("{error:#}"))
        };
        let _ = runtime_tx.send(RuntimeEvent::SettingsActionFinished {
            result,
            refresh: SettingsRefresh::Skills,
        });
    });
}

fn selected_provider_id(app: &TuiState) -> Option<String> {
    let settings = app.settings.as_ref()?;
    settings.provider_ids.get(settings.selected).cloned()
}

fn request_selected_provider_delete(app: &mut TuiState) {
    let Some(id) = selected_provider_id(app) else {
        return;
    };
    let Some(settings) = app.settings.as_mut() else {
        return;
    };
    if settings.document.providers.len() <= 1 {
        settings.error = Some("The last Provider cannot be deleted".into());
        return;
    }
    app.confirm_delete = Some(DeleteTarget::Provider(id));
}

fn request_selected_mcp_delete(app: &mut TuiState) {
    if let Some(item) = selected_settings_mcp(app) {
        if let Some(configured) = item.configured {
            app.confirm_delete = Some(DeleteTarget::Mcp(configured.name));
        } else {
            app.status = "This MCP is not installed".into();
        }
    }
}

fn request_selected_skill_delete(app: &mut TuiState) {
    if let Some(item) = selected_settings_skill(app) {
        if let Some(installed) = item.installed {
            app.confirm_delete = Some(DeleteTarget::Skill(installed.name));
        } else {
            app.status = "This Skill is not installed".into();
        }
    }
}

fn begin_provider_form(app: &mut TuiState, id: Option<String>) {
    let Some(settings) = app.settings.as_mut() else {
        return;
    };
    let provider = id
        .as_ref()
        .and_then(|id| settings.document.providers.get(id))
        .cloned()
        .unwrap_or_default();
    let fields = PROVIDER_FIELDS
        .iter()
        .copied()
        .map(|field| {
            let value = match field.key {
                "id" => id.clone().unwrap_or_default(),
                "display" => provider.display.clone(),
                "type" => normalized_provider_type(
                    &provider.provider_type,
                    provider.tool_protocol.as_deref(),
                )
                .into(),
                "tool_protocol" => {
                    normalized_tool_protocol(provider.tool_protocol.as_deref()).into()
                }
                "base_url" => provider.base_url.clone(),
                "model" => provider.model.clone(),
                "fast_model" => provider.fast_model.clone().unwrap_or_default(),
                "api_key" => provider.api_key.clone(),
                "context_window" => provider
                    .context_window
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "256000".into()),
                "effective_context_window_percent" => provider
                    .effective_context_window_percent
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                "auto_compact_token_limit" => provider
                    .auto_compact_token_limit
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                "auto_compact_scope" => match provider.auto_compact_scope {
                    coomi_engine::AutoCompactScope::Total => "total".into(),
                    coomi_engine::AutoCompactScope::BodyAfterPrefix => "body_after_prefix".into(),
                },
                "max_output_tokens" => provider
                    .max_output_tokens
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                "supports_remote_compaction" => provider
                    .supports_remote_compaction
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                "remote_compaction_mode" => match provider.remote_compaction_mode {
                    RemoteCompactionMode::Legacy => "legacy".into(),
                    RemoteCompactionMode::V2 => "v2".into(),
                },
                "supports_vision" => provider.supports_vision.to_string(),
                "supports_native_tools" => provider.supports_native_tools.to_string(),
                "supports_web_search" => provider.supports_web_search.to_string(),
                "supports_parallel_tool_calls" => provider.supports_parallel_tool_calls.to_string(),
                _ => String::new(),
            };
            let mut editor = Editor::default();
            editor.set(&value);
            (field, editor)
        })
        .collect();
    settings.error = None;
    settings.form = Some(ProviderForm {
        original_id: id,
        fields,
        selected: 0,
        choice_open: false,
        show_secret: false,
    });
}

fn cycle_provider_choice(form: &mut ProviderForm, delta: isize) {
    let options = form.fields[form.selected].0.options;
    if options.is_empty() {
        return;
    }
    let current = form.fields[form.selected].1.text();
    let index = options
        .iter()
        .position(|option| option.value == current)
        .unwrap_or(options.len() - 1);
    let next = if delta < 0 {
        index.checked_sub(1).unwrap_or(options.len() - 1)
    } else {
        (index + 1) % options.len()
    };
    set_provider_choice(form, next);
    if !form.choice_open && form.fields[form.selected].1.text() == CUSTOM_CONTEXT_WINDOW {
        form.fields[form.selected].1.clear();
    }
}

fn set_provider_choice(form: &mut ProviderForm, index: usize) {
    let field = form.fields[form.selected].0;
    if let Some(option) = field.options.get(index) {
        form.fields[form.selected].1.set(option.value);
    }
}

fn normalized_provider_type<'a>(provider_type: &'a str, tool_protocol: Option<&'a str>) -> &'a str {
    let normalized = provider_type.trim();
    match normalized {
        "openai_compatible" | "openai_responses" | "anthropic_messages" | "gemini_native" => {
            normalized
        }
        "openai" | "responses" => "openai_responses",
        "anthropic" => "anthropic_messages",
        "gemini" => "gemini_native",
        "generic" | "deepseek" | "chat_completions" | "" => tool_protocol
            .filter(|value| {
                PROVIDER_TYPE_OPTIONS
                    .iter()
                    .any(|option| option.value == *value)
            })
            .unwrap_or("openai_compatible"),
        _ => "openai_compatible",
    }
}

fn normalized_tool_protocol(tool_protocol: Option<&str>) -> &str {
    tool_protocol
        .filter(|value| {
            TOOL_PROTOCOL_OPTIONS
                .iter()
                .any(|option| option.value == *value)
        })
        .unwrap_or("auto")
}

fn save_provider_form(app: &mut TuiState) -> Result<()> {
    let Some(settings) = app.settings.as_mut() else {
        return Ok(());
    };
    let Some(form) = settings.form.as_ref() else {
        return Ok(());
    };
    let values = form
        .fields
        .iter()
        .map(|(field, editor)| (field.key, editor.text().trim().to_owned()))
        .collect::<BTreeMap<_, _>>();
    let id = values.get("id").cloned().unwrap_or_default();
    if id.is_empty() || id.contains(['/', '\\', ':']) {
        settings.error = Some("Provider ID is empty or contains an invalid path character".into());
        return Ok(());
    }
    if !PROVIDER_TYPE_OPTIONS
        .iter()
        .any(|option| option.value == values["type"])
    {
        settings.error = Some("Select a supported API compatibility mode".into());
        return Ok(());
    }
    if !TOOL_PROTOCOL_OPTIONS
        .iter()
        .any(|option| option.value == values["tool_protocol"])
    {
        settings.error = Some("Select a supported tool-calling mode".into());
        return Ok(());
    }
    let original_id = form.original_id.clone();
    let mut provider = original_id
        .as_ref()
        .and_then(|id| settings.document.providers.get(id))
        .cloned()
        .unwrap_or_else(ProviderSettings::default);
    provider.display = values["display"].clone();
    provider.provider_type = values["type"].clone();
    provider.tool_protocol = optional_string(&values["tool_protocol"]);
    provider.base_url = values["base_url"].clone();
    provider.model = values["model"].clone();
    provider.fast_model = optional_string(&values["fast_model"]);
    provider.api_key = values["api_key"].clone();
    provider.context_window = if values["context_window"].is_empty() {
        None
    } else {
        match values["context_window"].parse::<u64>() {
            Ok(value) => Some(value),
            Err(_) => {
                settings.error = Some("Context window must be a positive integer".into());
                return Ok(());
            }
        }
    };
    provider.effective_context_window_percent = match parse_optional_u8(
        &values["effective_context_window_percent"],
        "Effective window",
    ) {
        Ok(value) if value.is_none_or(|value| (1..=100).contains(&value)) => value,
        Ok(_) => {
            settings.error = Some("Effective window must be between 1 and 100".into());
            return Ok(());
        }
        Err(error) => {
            settings.error = Some(error);
            return Ok(());
        }
    };
    provider.auto_compact_token_limit =
        match parse_optional_u64(&values["auto_compact_token_limit"], "Compact token limit") {
            Ok(value) => value,
            Err(error) => {
                settings.error = Some(error);
                return Ok(());
            }
        };
    provider.auto_compact_scope = match values["auto_compact_scope"].as_str() {
        "" | "total" => coomi_engine::AutoCompactScope::Total,
        "body_after_prefix" => coomi_engine::AutoCompactScope::BodyAfterPrefix,
        _ => {
            settings.error = Some("Compact scope must be total or body_after_prefix".into());
            return Ok(());
        }
    };
    provider.max_output_tokens =
        match parse_optional_u64(&values["max_output_tokens"], "Max output tokens") {
            Ok(value) => value,
            Err(error) => {
                settings.error = Some(error);
                return Ok(());
            }
        };
    provider.supports_remote_compaction =
        match parse_optional_bool(&values["supports_remote_compaction"], "Remote compaction") {
            Ok(value) => value,
            Err(error) => {
                settings.error = Some(error);
                return Ok(());
            }
        };
    provider.remote_compaction_mode = match values["remote_compaction_mode"].as_str() {
        "" | "v2" => RemoteCompactionMode::V2,
        "legacy" => RemoteCompactionMode::Legacy,
        _ => {
            settings.error = Some("Remote mode must be v2 or legacy".into());
            return Ok(());
        }
    };
    for (key, label, target) in [
        ("supports_vision", "Vision", &mut provider.supports_vision),
        (
            "supports_native_tools",
            "Native tools",
            &mut provider.supports_native_tools,
        ),
        (
            "supports_web_search",
            "Native web search",
            &mut provider.supports_web_search,
        ),
        (
            "supports_parallel_tool_calls",
            "Parallel tools",
            &mut provider.supports_parallel_tool_calls,
        ),
    ] {
        match parse_bool(&values[key], label) {
            Ok(value) => *target = value,
            Err(error) => {
                settings.error = Some(error);
                return Ok(());
            }
        }
    }
    if values["tool_protocol"].eq_ignore_ascii_case("disabled") {
        provider.supports_native_tools = false;
    }

    let mut document = settings.document.clone();
    if let Some(original_id) = &original_id {
        document.providers.remove(original_id);
        if document.active == *original_id {
            document.active = id.clone();
        }
    } else if document.providers.contains_key(&id) {
        settings.error = Some(format!("Provider `{id}` already exists"));
        return Ok(());
    }
    document.providers.insert(id.clone(), provider);
    if document.active.is_empty() {
        document.active = id;
    }
    if let Err(error) = document.save(&app.home.join("config").join("providers.json")) {
        settings.error = Some(error.to_string());
        return Ok(());
    }
    settings.document = document;
    settings.provider_ids = settings.document.providers.keys().cloned().collect();
    settings.selected = settings
        .provider_ids
        .iter()
        .position(|candidate| candidate == &settings.document.active)
        .unwrap_or(0);
    settings.form = None;
    settings.error = None;
    app.models = load_registry(&app.home)?.choices();
    app.status = "Provider saved".into();
    Ok(())
}

fn activate_selected_provider(app: &mut TuiState) -> Result<()> {
    let Some(id) = selected_provider_id(app) else {
        return Ok(());
    };
    let settings = app.settings.as_mut().expect("settings");
    settings.document.active = id.clone();
    if let Err(error) = settings
        .document
        .save(&app.home.join("config").join("providers.json"))
    {
        settings.error = Some(error.to_string());
        return Ok(());
    }
    let registry = load_registry(&app.home)?;
    let provider = registry.resolve(Some(&id))?;
    app.models = registry.choices();
    app.session.switch_model(&provider.id, &provider.model);
    SessionStore::new(&app.home).save(&app.session)?;
    app.status = format!("Active provider: {}", provider.display);
    Ok(())
}

fn delete_provider(app: &mut TuiState, id: &str) -> Result<()> {
    let settings = app.settings.as_mut().expect("settings");
    if settings.document.providers.len() <= 1 {
        settings.error = Some("The last Provider cannot be deleted".into());
        return Ok(());
    }
    settings.document.providers.remove(id);
    if settings.document.active == id {
        settings.document.active = settings
            .document
            .providers
            .keys()
            .next()
            .cloned()
            .unwrap_or_default();
    }
    if let Err(error) = settings
        .document
        .save(&app.home.join("config").join("providers.json"))
    {
        settings.error = Some(error.to_string());
        return Ok(());
    }
    settings.provider_ids = settings.document.providers.keys().cloned().collect();
    settings.selected = settings
        .selected
        .min(settings.provider_ids.len().saturating_sub(1));
    app.models = load_registry(&app.home)?.choices();
    app.status = format!("Deleted provider {id}");
    Ok(())
}

fn optional_string(value: &str) -> Option<String> {
    (!value.trim().is_empty()).then(|| value.trim().to_owned())
}

fn parse_optional_u64(value: &str, label: &str) -> std::result::Result<Option<u64>, String> {
    if value.trim().is_empty() {
        return Ok(None);
    }
    value
        .trim()
        .parse::<u64>()
        .map(Some)
        .map_err(|_| format!("{label} must be a positive integer"))
}

fn parse_optional_u8(value: &str, label: &str) -> std::result::Result<Option<u8>, String> {
    if value.trim().is_empty() {
        return Ok(None);
    }
    value
        .trim()
        .parse::<u8>()
        .map(Some)
        .map_err(|_| format!("{label} must be a positive integer"))
}

fn parse_optional_bool(value: &str, label: &str) -> std::result::Result<Option<bool>, String> {
    if value.trim().is_empty() {
        return Ok(None);
    }
    parse_bool(value, label).map(Some)
}

fn parse_bool(value: &str, label: &str) -> std::result::Result<bool, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "1" | "on" => Ok(true),
        "false" | "no" | "0" | "off" => Ok(false),
        _ => Err(format!("{label} must be true or false")),
    }
}

fn submit_mcp_form(app: &mut TuiState) -> Result<()> {
    let Some(form) = &app.mcp_form else {
        return Ok(());
    };
    let values = form
        .fields
        .iter()
        .map(|(parameter, editor)| (parameter.key.clone(), editor.text()))
        .collect::<BTreeMap<_, _>>();
    let path = CatalogInstaller::new(&app.home).install_mcp(&form.entry.id, &values)?;
    let name = form.entry.name.clone();
    app.close_overlay();
    app.push_notice(
        NoticeKind::Success,
        format!("Configured {name} at {}", path.display()),
    );
    Ok(())
}

fn model_matches(choice: &ModelChoice, query: &str) -> bool {
    query.is_empty()
        || choice.selector.to_ascii_lowercase().contains(query)
        || choice.model.to_ascii_lowercase().contains(query)
        || choice.provider_display.to_ascii_lowercase().contains(query)
}

fn session_matches(session: &SessionSummary, query: &str) -> bool {
    query.is_empty()
        || session.preview.to_ascii_lowercase().contains(query)
        || session.title.to_ascii_lowercase().contains(query)
        || session.summary.to_ascii_lowercase().contains(query)
        || session.model.to_ascii_lowercase().contains(query)
        || session.id.to_string().contains(query)
}

fn catalog_matches(id: &str, name: &str, query: &str) -> bool {
    query.is_empty()
        || id.to_ascii_lowercase().contains(query)
        || name.to_ascii_lowercase().contains(query)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn test_state() -> (tempfile::TempDir, TuiState) {
        let home = tempfile::tempdir().expect("temporary home");
        let config = home.path().join("config");
        fs::create_dir(&config).expect("create config");
        fs::write(
            config.join("providers.json"),
            r#"{"active":"demo","providers":{"demo":{"type":"generic","display":"Demo","api_key":"","base_url":"http://localhost/v1","model":"demo-model"}}}"#,
        )
        .expect("write providers");
        let cwd = home.path().canonicalize().expect("canonical home");
        let cli = Cli {
            home: None,
            cwd: cwd.clone(),
            model: None,
            policy: AccessMode::WorkspaceWrite,
            yes: false,
            command: None,
        };
        let paths = RuntimePaths {
            home: cwd.clone(),
            cwd: cwd.clone(),
        };
        let session = Session::new("demo", "demo-model", cwd);
        let state = TuiState::new(&cli, &paths, session).expect("TUI state");
        (home, state)
    }

    #[test]
    fn shortcuts_open_coomi_overlays() {
        let (_home, mut state) = test_state();
        let (sender, _receiver) = mpsc::unbounded_channel();

        handle_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL),
            sender,
        )
        .expect("handle shortcut");

        assert_eq!(
            state.overlay.as_ref().map(|overlay| overlay.kind),
            Some(OverlayKind::Commands)
        );
    }

    #[test]
    fn arrow_events_recall_input_and_page_keys_scroll_timeline() {
        let (_home, mut state) = test_state();
        let (sender, _receiver) = mpsc::unbounded_channel();
        state.timeline_max_scroll.set(40);
        state.follow_tail = true;
        state.input_history.push("previous prompt".into());

        // Up/Down with empty editor → recall input history
        handle_key(&mut state, KeyEvent::from(KeyCode::Up), sender.clone()).expect("recall input");
        assert_eq!(state.editor.text(), "previous prompt");
        assert!(state.follow_tail); // scroll unchanged

        // PageUp/PageDown always scroll the timeline
        handle_key(&mut state, KeyEvent::from(KeyCode::PageUp), sender.clone())
            .expect("scroll timeline");
        assert_eq!(state.scroll, 32);
        assert!(!state.follow_tail);

        // Alt+Up/Down also recalls input
        state.editor.clear();
        handle_key(
            &mut state,
            KeyEvent::new(KeyCode::Up, KeyModifiers::ALT),
            sender,
        )
        .expect("alt recall input");
        assert_eq!(state.editor.text(), "previous prompt");
    }

    #[test]
    fn active_turn_protects_session_and_scroll_tracks_distance_from_tail() {
        let (_home, mut state) = test_state();
        let session_id = state.session.id;
        state.busy = true;

        new_session(&mut state).expect("new session is gated");
        assert_eq!(state.session.id, session_id);
        assert!(state.status.contains("active turn"));

        state.follow_tail = true;
        state.timeline_max_scroll.set(40);
        state.scroll_up(8);
        assert_eq!(state.scroll, 32);
        assert!(!state.follow_tail);
        state.scroll_down(8);
        assert_eq!(state.scroll, 40);
        assert!(state.follow_tail);
    }

    #[test]
    fn streaming_deltas_preserve_a_scrolled_viewport() {
        let (_home, mut state) = test_state();
        state.active_generation = Some(7);
        state.busy = true;
        state
            .timeline
            .push(TimelineEntry::Assistant("first".into()));
        state.timeline_max_scroll.set(40);
        state.follow_tail = false;
        state.scroll = 12;

        handle_runtime_event(
            &mut state,
            RuntimeEvent::Agent {
                generation: 7,
                event: AgentEvent::TextDelta(" second".into()),
            },
        );

        assert!(!state.follow_tail);
        assert_eq!(state.scroll, 12);
        assert!(matches!(
            state.timeline.last(),
            Some(TimelineEntry::Assistant(text)) if text == "first second"
        ));
    }

    #[test]
    fn slash_status_and_memory_are_control_plane_commands() {
        let (_home, mut state) = test_state();
        let (sender, _receiver) = mpsc::unbounded_channel();

        execute_slash_command(&mut state, "/status", sender.clone()).expect("status command");
        execute_slash_command(
            &mut state,
            "/memory add Prefer concise output",
            sender.clone(),
        )
        .expect("add memory");
        execute_slash_command(&mut state, "/memory list", sender).expect("list memory");

        assert!(state.session.messages.is_empty());
        assert!(state.timeline.iter().any(|entry| {
            matches!(entry, TimelineEntry::Notice { text, .. } if text.contains("persistent memory item"))
        }));
    }

    #[test]
    fn provider_capability_values_are_validated() {
        assert_eq!(parse_bool("ON", "feature"), Ok(true));
        assert_eq!(parse_bool("false", "feature"), Ok(false));
        assert!(parse_bool("sometimes", "feature").is_err());
        assert_eq!(parse_optional_u64("128000", "window"), Ok(Some(128_000)));
    }

    #[test]
    fn provider_choices_open_and_follow_arrow_selection() {
        let (_home, mut state) = test_state();
        state.open_overlay(OverlayKind::Settings);
        begin_provider_form(&mut state, Some("demo".into()));
        let (sender, _receiver) = mpsc::unbounded_channel();
        let form = state
            .settings
            .as_mut()
            .and_then(|settings| settings.form.as_mut())
            .expect("provider form");
        form.selected = 2;
        handle_settings_key(&mut state, KeyEvent::from(KeyCode::Enter), sender.clone())
            .expect("open API compatibility choices");
        handle_settings_key(&mut state, KeyEvent::from(KeyCode::Down), sender.clone())
            .expect("select next API compatibility");
        handle_settings_key(&mut state, KeyEvent::from(KeyCode::Enter), sender)
            .expect("close API compatibility choices");
        let value = state
            .settings
            .as_ref()
            .and_then(|settings| settings.form.as_ref())
            .and_then(|form| form.fields.get(2))
            .map(|(_, editor)| editor.text());
        assert_eq!(value.as_deref(), Some("openai_responses"));
    }

    #[test]
    fn context_window_defaults_to_256k_and_supports_custom_input() {
        let (_home, mut state) = test_state();
        state.open_overlay(OverlayKind::Settings);
        begin_provider_form(&mut state, Some("demo".into()));
        let (sender, _receiver) = mpsc::unbounded_channel();
        let form = state
            .settings
            .as_mut()
            .and_then(|settings| settings.form.as_mut())
            .expect("provider form");
        form.selected = 8;
        assert_eq!(form.fields[8].1.text(), "256000");

        handle_settings_key(&mut state, KeyEvent::from(KeyCode::Right), sender.clone())
            .expect("select 512K");
        handle_settings_key(&mut state, KeyEvent::from(KeyCode::Right), sender.clone())
            .expect("select custom input");
        for character in "300000".chars() {
            handle_settings_key(
                &mut state,
                KeyEvent::from(KeyCode::Char(character)),
                sender.clone(),
            )
            .expect("type custom context window");
        }
        let form = state
            .settings
            .as_ref()
            .and_then(|settings| settings.form.as_ref())
            .expect("provider form");
        assert_eq!(form.fields[8].1.text(), "300000");
    }

    #[test]
    fn approval_can_be_selected_with_arrows_and_confirmed() {
        let (_home, mut state) = test_state();
        let (responder, mut receiver) = oneshot::channel();
        state.pending_approval = Some(PendingApproval {
            call: ToolCall {
                id: "call-1".into(),
                name: "local_shell".into(),
                arguments: serde_json::json!({"command": "cargo test"}),
            },
            reason: "Run the test suite?".into(),
            selected: ApprovalChoice::Approve,
            responder: Some(responder),
        });
        handle_approval_key(&mut state, KeyEvent::from(KeyCode::Right)).expect("select deny");
        assert_eq!(
            state
                .pending_approval
                .as_ref()
                .map(|pending| pending.selected),
            Some(ApprovalChoice::Deny)
        );
        handle_approval_key(&mut state, KeyEvent::from(KeyCode::Enter)).expect("confirm deny");
        assert!(state.pending_approval.is_none());
        assert!(!receiver.try_recv().expect("approval response"));
    }

    #[test]
    fn user_questions_support_all_arrow_keys_and_enter() {
        let (_home, mut state) = test_state();
        let (sender, _receiver) = oneshot::channel();
        state.pending_user_input = Some(PendingUserInput {
            request: UserInputRequest {
                questions: vec![coomi_engine::UserInputQuestion {
                    id: "scope".into(),
                    header: "Scope".into(),
                    question: "Choose a scope".into(),
                    options: vec![
                        coomi_engine::UserInputOption {
                            label: "Workspace".into(),
                            description: "Current workspace".into(),
                        },
                        coomi_engine::UserInputOption {
                            label: "Global".into(),
                            description: "All workspaces".into(),
                        },
                    ],
                }],
                auto_resolution_ms: None,
            },
            question_index: 0,
            option_index: 1,
            other_editor: None,
            answers: BTreeMap::new(),
            responder: Some(sender),
        });

        handle_user_input_key(&mut state, KeyEvent::from(KeyCode::Left)).expect("left");
        assert_eq!(
            state
                .pending_user_input
                .as_ref()
                .map(|pending| pending.option_index),
            Some(0)
        );
        handle_user_input_key(&mut state, KeyEvent::from(KeyCode::Right)).expect("right");
        handle_user_input_key(&mut state, KeyEvent::from(KeyCode::Up)).expect("up");
        handle_user_input_key(&mut state, KeyEvent::from(KeyCode::Down)).expect("down");
        assert_eq!(
            state
                .pending_user_input
                .as_ref()
                .map(|pending| pending.option_index),
            Some(1)
        );
        handle_user_input_key(&mut state, KeyEvent::from(KeyCode::Enter)).expect("enter");
        assert!(state.pending_user_input.is_none());
    }

    #[test]
    fn settings_merge_curated_catalogs_and_confirm_installed_deletions() {
        let (_home, mut state) = test_state();
        state.open_overlay(OverlayKind::Settings);
        assert_eq!(settings_mcp_items(&state).len(), 6);
        assert_eq!(settings_skill_items(&state).len(), 8);

        let settings = state.settings.as_mut().expect("settings");
        settings.tab = SettingsTab::Mcp;
        settings.mcp_servers.push(ConfiguredMcp {
            name: "fetch".into(),
            transport: "stdio".into(),
            enabled: true,
            target: "uvx".into(),
        });
        settings.selected = state
            .mcp_entries
            .iter()
            .position(|entry| entry.id == "fetch")
            .expect("fetch entry");
        request_selected_mcp_delete(&mut state);
        assert_eq!(
            state.confirm_delete,
            Some(DeleteTarget::Mcp("fetch".into()))
        );
    }
}
