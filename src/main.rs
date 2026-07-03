use anyhow::Result;
use clap::Parser;

use claude_go::cli::{run, Cli};

fn main() -> Result<()> {
    // Parse args first so we know whether the user gave an explicit
    // subcommand. The TTY gate only applies to the no-args default
    // (and to the explicit `tui` subcommand when invoked without a
    // TTY).
    let cli = Cli::parse();
    let has_subcommand = cli.cmd.is_some();
    let tty_ok = claude_go::tty::should_launch_tui();

    if !has_subcommand && !tty_ok {
        // No subcommand, no TTY: print status JSON and exit. Scripts
        // can `claude-go | jq '.enabled'` to branch.
        let paths = claude_go::paths::Paths::resolve();
        let code = claude_go::tty::print_status_json(&paths);
        std::process::exit(code);
    }

    // The in-process proxy needs a tokio runtime, so build one and
    // run the CLI inside it. We use `block_on` so the rest of the
    // code can stay synchronous; the proxy's own task lives until
    // the process exits.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let code = runtime.block_on(run(cli))?;
    std::process::exit(code);
}
