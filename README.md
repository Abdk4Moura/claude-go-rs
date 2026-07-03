# claude-go

Route Claude Code to any Anthropic-compatible model with a beautiful TUI.

```text
claude-go  /  provider                                                  1  .  2  .  3
──────────────────────────────────────────────────────────────────────
 >>    OpenCode Go             [anthropic]  https://opencode.ai/zen/go
       Anthropic direct        [anthropic]  https://api.anthropic.com
       OpenRouter              [anthropic]  https://openrouter.ai/api/v1
    !  Cloudflare AI Gateway   [anthropic]  https://gateway.ai.cloudflare.com/v1
    !  Google Vertex (Claude)  [anthropic]  https://aiplatform.googleapis.com
    !  AWS Bedrock (Claude)    [anthropic]  https://bedrock-runtime.us-east-1.amazonaws.com
       Custom URL...           [anthropic]  ...
──────────────────────────────────────────────────────────────────────
j/k or arrows  move     Enter  select     a  add custom     d  remove custom
```

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/Abdk4Moura/claude-go/main/install.sh | bash
```

This downloads the right binary for your OS/arch from the latest GitHub
release and installs it to `~/.local/bin/claude-go`. Make sure
`~/.local/bin` is on your `PATH`.

Want a specific version?

```sh
curl -fsSL https://raw.githubusercontent.com/Abdk4Moura/claude-go/main/install.sh | bash -s -- v0.1.0
```

## Quick start

```sh
# 1. Get an API key (https://opencode.ai/auth), then:
export OPENCODE_API_KEY=sk-...

# 2. Launch the TUI
claude-go

# 3. Pick a provider, pick a model, and you're routed.
```

Or stay on the command line:

```sh
claude-go on --model minimax-m3
claude-go status
claude-go verify
claude-go off
```

## What it does

`claude-go` writes the right `ANTHROPIC_*` env vars into
`~/.claude/settings.json` so Claude Code routes to a different model. It
owns a small, well-defined slice of that file (9 env keys: 8 owned + a marker)
and touches nothing else -- your other env vars, permissions, theme,
plugins, MCP servers, and hooks stay put.

Two endpoint shapes are supported out of the box:

- **Anthropic-format** (e.g. OpenCode Go's `/v1/messages`, direct
  Anthropic, OpenRouter): direct, no proxy.
- **OpenAI Chat Completions format** (e.g. GLM, Kimi, DeepSeek via
  OpenCode Go): routed through an **in-process** translation proxy
  that `claude-go` itself hosts inside the same binary (no Node, no
  npm, no separate process).

The proxy is a `tokio::task` in the same process: `claude-go on`
binds it to `127.0.0.1:0` (OS-picks the port), writes the bound
port into `~/.claude/settings.json`, and blocks (for OpenAI-format
models) so the proxy stays alive. Send Ctrl-C to stop it, or run
`claude-go off` in the same terminal. **The proxy is gone when the
process exits**, so for OpenAI-format models, keep the `claude-go
on` terminal open while Claude Code is running.

## Architecture

```text
Claude Code
    |
    |  Anthropic Messages API
    v
+--------------------+
| claude-go          |
| (settings.json)    |
+--------------------+
    |
    |  Anthropic-format  -> direct to provider
    |  OpenAI-format     -> in-process axum server (localhost:0)
    v
+--------------------+    +------------------+
| Provider           |    | claude-go proxy  |
| (OpenCode Go,      |    | (axum, in-proc)  |
|  OpenRouter, ...)  |    | Anthropic<->OpenAI|
+--------------------+    +------------------+
```

`~/.claude/settings.json` is the only thing Claude Code reads. `claude-go`
writes a 9-key env block with an `__claude_go_owned: "1"` marker so
`off` only strips its own keys and never destroys a user's own
`ANTHROPIC_*` setup.

## TUI

Three screens, navigated with Tab / arrow keys + Enter. Quit with `q`
or `Ctrl-C`.

| Screen | Purpose |
|--------|---------|
| 1 / provider | Pick a built-in preset (OpenCode Go, Anthropic direct, OpenRouter, ...) or add a custom URL. |
| 2 / model | Pick a model. For OpenCode Go the list is live-fetched from `/v1/models` (5 min cache) and falls back to the hardcoded 19 models. For other providers, type any model id. |
| 3 / status | Big ON / OFF indicator + live state (settings.json path, proxy state, last verify result). `o` toggles, `v` runs verify, `r` refreshes. |

The TUI is a real TUI. It uses crossterm + ratatui, supports any
terminal width, and degrades to a clean CLI for scripting.

## CLI

```
claude-go                          # launch the TUI
claude-go on [--model M] [--port P] # enable
claude-go off                      # disable
claude-go status                   # show current state
claude-go verify                   # round-trip test
claude-go models                   # list 19 hardcoded models
claude-go providers                # list configured providers
claude-go provider add NAME --url URL [--auth-header H]
claude-go provider remove NAME
claude-go install                  # install to ~/.local/bin
claude-go help                     # help
claude-go version                  # version
```

## Custom providers

```sh
claude-go provider add my-corp --url https://llm.internal.corp
claude-go provider remove my-corp
```

Persists to `~/.config/claude-go/providers.json`. Custom providers
can be removed from the TUI with `d`.

## Files

| Path | What |
|------|------|
| `~/.claude/settings.json` | Claude Code's settings (claude-go owns a 9-key env block) |
| `~/.local/share/claude-go/` | State dir (marker file only; no per-proxy files) |
| `~/.config/claude-go/providers.json` | Custom provider registry |
| `~/.local/bin/claude-go` | Default install path |

## Requirements

- Linux (x86_64 or aarch64), macOS (x86_64 or Apple Silicon), or
  Windows (x86_64)
- No Node.js, no npm. The OpenAI-format translation proxy runs
  in-process in v0.2.0.
- `OPENCODE_API_KEY` in your environment for OpenCode Go

## Caveats

- **TTY-aware no-args default.** In a real terminal, `claude-go`
  launches the TUI. Outside a TTY (pipes, redirects, `nohup`, `cron`,
  `systemd`, Docker, `make`, etc.), `claude-go` with no args prints
  the current state as one JSON object on stdout and exits 0, so
  scripts can `claude-go | jq '.enabled'` to branch. The explicit
  `claude-go tui` subcommand works the same way and prints a hint if
  invoked without a TTY.
- **The in-process proxy is process-lifetime scoped.** For
  OpenAI-format models, `claude-go on` blocks; the proxy is killed
  when that process exits. Run Claude Code in a separate terminal
  while `claude-go on` is alive.
- Sub-tasks (haiku/sonnet/opus routing) all use the main model. There's
  no per-subtask dispatch in this tool.
- Cloudflare, Vertex, and Bedrock presets are listed in the TUI but
  show "not yet implemented". OpenCode Go, Anthropic direct, and
  OpenRouter are fully working in v0.2.0.

## v0.2.0 changes (from v0.1.0)

- **TTY-safe launch.** `claude-go` no longer crashes in non-TTY
  contexts. Prints status JSON for scripts; prints the TTY hint
  when the `tui` subcommand is invoked without a TTY.
- **Self-contained binary.** Dropped the Node.js + `opencode-api`
  dependency. The translation proxy now runs as a `tokio::task`
  inside the same binary (`axum`-based HTTP server bound to
  `127.0.0.1:0`). No more `npm install`, no more separate
  process, no more PID or port files on disk.
- **Windows support.** axum 0.7 builds on Windows; the CI matrix
  adds `x86_64-pc-windows-msvc`.
- **Smaller memory footprint.** No Node runtime. ~10 MB resident
  (was ~30 MB with Node + `opencode-api`).
- **Faster proxy startup.** ~30 ms (was ~500 ms Node cold start).

## v0.2.2 changes (from v0.2.1)

v0.2.0 introduced a tokio panic in the no-args TUI launch path that
was not caught by the v0.2.0 / v0.2.1 smoke tests because those tests
verified only the CLI subcommand path, not the TUI entrypoint. The
TUI's `App::new` built a *second* tokio runtime and called `block_on`
on it from inside the outer runtime that `main.rs` had already
started, which panics with "Cannot start a runtime from within a
runtime". v0.2.2 fixes the panic (the blocking call now uses
`tokio::task::block_in_place` on the existing multi-thread runtime
instead of constructing a nested one) and adds a regression test
(`tests/tui_launch_regression.rs`) that launches the no-args path
headlessly and asserts the marker file gets written, so a TUI crash
can never ship undetected again.

## Development

```sh
cargo build --release
cargo test
cargo run -- status
cargo run -- models
```

The dev binary is at `target/release/claude-go`. Run it directly while
iterating; the TUI works in any modern terminal.

## License

MIT. See [LICENSE](./LICENSE).
