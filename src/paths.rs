use std::path::PathBuf;

use directories::ProjectDirs;

/// Resolves all on-disk paths used by claude-go. Resolution is centralized
/// here so the rest of the code does not have to think about $HOME, XDG,
/// or platform differences.
#[derive(Debug, Clone)]
pub struct Paths {
    /// `~/.claude/settings.json`
    pub settings_file: PathBuf,
    /// `~/.local/share/claude-go/`
    pub state_dir: PathBuf,
    /// `~/.config/claude-go/providers.json`
    pub providers_file: PathBuf,
    /// `~/.local/bin/claude-go`
    pub install_path: PathBuf,
}

impl Paths {
    /// Resolve paths using the standard `$HOME`-based layout that matches
    /// the bash v0.1.1 contract. We intentionally do not use the
    /// `directories` crate's XDG rewrites here so the on-disk shape stays
    /// bit-for-bit compatible with the bash version.
    pub fn resolve() -> Self {
        let home = directories::UserDirs::new()
            .map(|u| u.home_dir().to_path_buf())
            .or_else(detect_home_from_env)
            .unwrap_or_else(|| PathBuf::from("."));

        let state_dir = home.join(".local/share/claude-go");
        let config_dir = ProjectDirs::from("com", "Abdk4Moura", "claude-go")
            .map(|p| p.config_dir().to_path_buf())
            .unwrap_or_else(|| home.join(".config/claude-go"));

        Self {
            settings_file: home.join(".claude/settings.json"),
            state_dir,
            providers_file: config_dir.join("providers.json"),
            install_path: home.join(".local/bin/claude-go"),
        }
    }

    /// Override `HOME` for tests. Builds a Paths with every entry rooted
    /// at `home`. Used by integration tests.
    pub fn resolve_under(home: &std::path::Path) -> Self {
        let state_dir = home.join(".local/share/claude-go");
        Self {
            settings_file: home.join(".claude/settings.json"),
            state_dir,
            providers_file: home.join(".config/claude-go/providers.json"),
            install_path: home.join(".local/bin/claude-go"),
        }
    }
}

fn detect_home_from_env() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}
