# Coomi Rust

Coomi is a compact terminal coding agent organized as six workspace layers.

| Layer | Responsibility |
| --- | --- |
| `engine` | Agent loop, canonical messages, tool lifecycle, sessions, project instructions |
| `services` | Provider registry and OpenAI-compatible, Responses, Anthropic, and Gemini adapters |
| `tools` | File reading/writing/editing, search, and shell execution |
| `security` | Workspace boundaries, access profiles, destructive-command policy |
| `ui` | Interactive terminal, non-interactive execution, model and session commands |
| `catalogs` | Built-in installable MCP and Skill catalogs |

Installed Skills are exposed to the agent through `list_skills` and
`read_skill`, so full instructions are loaded only when a task needs them.

## Agent foundation

The runtime now carries the mature agent core used by Coomi Android:

- **Agent loop**: transient provider failures retry with bounded backoff; image
  protocol failures retry the same round without images; invalid tool arguments
  get one correction round; repeated failing calls are blocked until their
  arguments change.
- **Execution**: independent read-only tools run concurrently, while mutations
  and shells are serialized through a process-wide gate. Hosts can supply turn
  control, and queued input is drained at model and tool boundaries.
- **Recovery**: the TUI checkpoints the session after every durable step, so an
  interrupted turn leaves a complete transcript on disk. Hosts can also use
  `Agent::continue_interrupted_turn` and `Agent::check_task_completion` for
  resumable or verified turns.
- **Context**: compaction retains a full archive, summarizes oversized histories
  in bounded chunks, supports remote Responses compaction with a local fallback,
  and prunes oversized tool previews only in request copies.
- **File history**: `write_file`, `edit_file`, and `apply_patch` record exact
  before/after bytes under `<home>/turn-files/` for the current user turn.
  Restoration is exposed as a service; shell side effects are explicitly marked
  untracked, and MCP/external effects stay outside the journal.
- **Sessions**: atomic writes, per-session titles, summaries and pinning, message
  edit/delete/truncate, and archive persistence.
- **Supporting services**: `capabilities` (model capability registry and probes),
  `skill_router` (keyword/file/tool/project routing with reference safety),
  `task_manager` (durable task queue with resource leases and conflict
  baselines), and workflows (dependency-aware definitions plus a runner).

Android-only runtime pieces (Termux/PRoot management, Shizuku, local
model/OCR/TTS helpers, and DeepSeek account login) are intentionally absent from
the desktop runtime. Use an OpenAI-compatible DeepSeek endpoint instead.

## Provider configuration

Models are loaded exclusively from `~/.coomi/config/providers.json` (or
`$COOMI_HOME/config/providers.json`). Both `model` and `fast_model` are exposed
by `coomi models`; arbitrary model overrides are rejected.

```json
{
  "version": 1,
  "active": "my-provider",
  "providers": {
    "my-provider": {
      "type": "openai_compatible",
      "display": "My Provider",
      "api_key": "...",
      "base_url": "https://example.com/v1",
      "model": "main-model",
      "fast_model": "fast-model",
      "context_window": 256000,
      "effective_context_window_percent": 95,
      "auto_compact_scope": "total",
      "supports_web_search": false,
      "supports_vision": false
    }
  }
}
```

Supported provider types are `openai_compatible`, `openai_responses`,
`anthropic_messages`, and `gemini_native`. The legacy value `generic` maps to
`openai_compatible`.

Provider API keys remain plain text in `providers.json`. Settings masks them by
default and allows temporary reveal with `V`; it does not claim to encrypt the
file. OpenAI Responses providers can opt into trigger-based remote compaction with
`"supports_remote_compaction": true` and
`"remote_compaction_mode": "v2"`. Set the mode to `"legacy"` only for a
provider that implements `/responses/compact` instead of `compaction_trigger`.

## Context management

Coomi uses provider usage as the context baseline and estimates local changes
incrementally. Tool calls remain paired with outputs, oversized outputs are
trimmed before old history, and real user
messages are retained around a compacted checkpoint. Context-window errors also
trigger one compaction and retry.

Automatic compaction is adaptive. Its default ceiling is 90% of the model's
declared context window, bounded by the effective context window and optionally
lowered by `auto_compact_token_limit`. It is therefore not a separate mechanism
that waits blindly for a UI meter to reach 90%. Use `body_after_prefix` for
providers whose reusable prefix should be excluded from the compaction counter.
Use `/compact` to run the same compactor immediately.

## Commands

```text
coomi
coomi exec "inspect this repository"
coomi models
coomi sessions
coomi resume --last
coomi compact --last
coomi catalog list mcp
coomi catalog list skill
coomi catalog install mcp filesystem --set allowed_path=F:\Projects\Demo
coomi catalog install skill frontend-design
```

Running `coomi` or an interactive `coomi resume` opens the full-screen terminal
interface. It keeps the conversation timeline, tool progress, queued follow-ups,
and multiline composer visible together. Use `Ctrl+K` for the command palette,
`Ctrl+R` for workspace session history, `Alt+M` for configured models, `Alt+S`
for Settings, and `Alt+H` for the complete key reference. `Alt+L` opens or edits
the active Loop, `Alt+I` starts a read-only Side Session, `Shift+Tab` cycles
the access policy, and `Esc` cancels an active turn. No F1-F12 binding is used.

Typing `/` opens the command picker. The main control commands are `/status`,
`/compact`, `/model`, `/history`, `/loop`, `/plan`, `/memory`, `/mcp`, `/skills`,
`/settings`, `/catalog`, `/new`, `/clear`, and `/quit`. Approval prompts and user
questions are bottom sheets directly above the composer, not centered dialogs.
Assistant output is rendered as Markdown, including headings, lists, tables,
links, code spans, and fenced code blocks. The footer shows active context usage.

## Runtime capabilities

The built-in tool surface includes `local_shell`, `read_file`, `write_file`,
`edit_file`, `apply_patch`, `list_dir`, `grep_files`, `web_search`, `view_image`,
`request_user_input`, `spawn_agent`, `wait_agent`, `close_agent`, and
`update_plan`, plus Loop, Memory, Skill, MCP management (`list_mcp`,
`uninstall_skill`, `uninstall_mcp`), workflow definition, process, and configured
MCP tools.

Provider-native web search replaces the local fallback when declared. Image tool
results use native multimodal content for Responses, Chat Completions, Anthropic,
and Gemini instead of placing Base64 in ordinary text.

Busy-turn input is accepted into the active model/tool loop at the next model or
tool boundary. Memory, Hooks, Side Sessions, automatic pasted Provider/MCP/Skill
configuration, startup update checks, installable catalogs, and keyboard-only
Settings are available in the first interactive session.

The default `workspace-write` policy allows file edits inside the selected
working directory and asks before arbitrary shell commands. `read-only` blocks
writes; `full-access` expands the path boundary but still asks before commands
recognized as destructive unless `--yes` is explicitly supplied.
