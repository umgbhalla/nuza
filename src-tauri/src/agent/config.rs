//! Which agent nuza starts, and exactly how.
//!
//! The command line for every preset is built here, from this table, and from
//! nowhere else: the webview names an agent, it never supplies a program or
//! an argument for one. A custom agent is the exception, and it cannot be
//! started without the person confirming that exact command line in a dialog
//! the webview does not draw (see `store.rs`).
//!
//! The adapter versions are pinned exactly. The adapters are separate
//! packages from the agents they wrap and can lag or break against a new
//! agent release; moving a pin is a release of its own, made after a real
//! turn has been run through the new version. `src/lib/ai/agents.ts` shows
//! the same pins in Settings, and a test keeps the two in step.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The Claude Code adapter, run through `npx`.
pub(crate) const CLAUDE_ADAPTER: &str = "@agentclientprotocol/claude-agent-acp@0.90.0";
/// The Codex adapter, run through `npx`.
pub(crate) const CODEX_ADAPTER: &str = "@agentclientprotocol/codex-acp@2.2.2";

/// The npm cache the Claude adapter is fetched into. Its own, because one
/// root-owned folder in a shared `~/.npm` - the usual legacy of an old
/// `sudo npm install` - makes `npx` fail with an error that reads as nuza's.
const CLAUDE_NPM_CACHE: &str = "~/.npm/_nuza-claude";
/// The same for the Codex adapter: a cache that is nuza's alone is also one
/// nuza can repair after a download was stopped part way (see `start.rs`).
const CODEX_NPM_CACHE: &str = "~/.npm/_nuza-codex";

/// Programs that run a script, so the script is what the agent is called.
const INTERPRETERS: [&str; 7] = ["node", "nodejs", "bun", "deno", "python", "python3", "npx"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum AgentConfig {
    ClaudeCode,
    Codex,
    OpenCode,
    /// Any program that speaks ACP on its stdin and stdout.
    #[serde(rename_all = "camelCase")]
    Custom {
        /// A name on `PATH`, or a path (`~/` allowed).
        command: String,
        args: Vec<String>,
        /// Where it keeps its own state - a sign-in, a cache - which the
        /// sandbox lets it read and write, and nothing else.
        data_dirs: Vec<String>,
    },
}

/// How an agent is started, once its config has been read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Launch {
    /// The program, as typed: resolved against `PATH` at the moment of
    /// starting, not here.
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
    /// Folders inside home the agent may read and write.
    pub(crate) state_dirs: Vec<PathBuf>,
    /// Files inside home the agent may read and write, matched as a prefix -
    /// `~/.claude.json` is rewritten through `~/.claude.json.backup` and
    /// friends.
    pub(crate) state_files: Vec<PathBuf>,
    /// Folders inside home the agent may read but not write: its own program,
    /// when that lives in home.
    pub(crate) read_dirs: Vec<PathBuf>,
    /// Whether the agent may read the home folder's own directory entry - its
    /// listing, and nothing inside it. OpenCode resolves the home folder
    /// itself while it starts, and refuses to run at all when that read is
    /// refused, so this is the smallest hole that lets it work.
    pub(crate) home_readable: bool,
}

impl AgentConfig {
    /// A stable name for the agent, used to remember consent. A custom agent's
    /// includes everything that decides what runs, so changing a single
    /// argument has to be agreed to again.
    pub(crate) fn id(&self) -> String {
        match self {
            AgentConfig::ClaudeCode => "claude-code".to_string(),
            AgentConfig::Codex => "codex".to_string(),
            AgentConfig::OpenCode => "opencode".to_string(),
            AgentConfig::Custom {
                command,
                args,
                data_dirs,
            } => {
                let described =
                    serde_json::to_string(&(command.trim(), args, data_dirs)).unwrap_or_default();
                format!("custom:{described}")
            }
        }
    }

    /// What to call the agent in anything the person reads: the product's
    /// name, or the program's own name for one they set up themselves.
    pub(crate) fn label(&self) -> String {
        match self {
            AgentConfig::ClaudeCode => "Claude Code".to_string(),
            AgentConfig::Codex => "Codex".to_string(),
            AgentConfig::OpenCode => "OpenCode".to_string(),
            AgentConfig::Custom { command, args, .. } => {
                let name_of = |path: &str| {
                    Path::new(path.trim())
                        .file_stem()
                        .map(|name| name.to_string_lossy().into_owned())
                        .filter(|name| !name.is_empty())
                };
                let program = name_of(command);
                // `node ~/agents/mine.js` is "mine", not "node": an
                // interpreter is not what anyone thinks of as the agent.
                let script = args.first().filter(|_| {
                    program
                        .as_deref()
                        .is_some_and(|program| INTERPRETERS.contains(&program))
                });
                script
                    .and_then(|script| name_of(script))
                    .or(program)
                    .unwrap_or_else(|| "The agent".to_string())
            }
        }
    }

    /// Whether the agent is reached through `npx`, and so needs Node.
    pub(crate) fn needs_node(&self) -> bool {
        matches!(self, AgentConfig::ClaudeCode | AgentConfig::Codex)
    }

    /// Whether a session that cannot be made read-only must not be used at
    /// all. Codex has the mode and an unrestricted default, so failing to
    /// switch is failing safe; the others have no such mode to fail on.
    pub(crate) fn requires_read_only(&self) -> bool {
        matches!(self, AgentConfig::Codex)
    }

    /// Whether models are changed with `session/set_model` (Codex) rather than
    /// as a `model` config option (everything else).
    pub(crate) fn sets_model_directly(&self) -> bool {
        matches!(self, AgentConfig::Codex)
    }

    /// The npm cache that is nuza's own, for an agent fetched through `npx`.
    pub(crate) fn npm_cache(&self, home: Option<&Path>) -> Option<PathBuf> {
        let cache = match self {
            AgentConfig::ClaudeCode => CLAUDE_NPM_CACHE,
            AgentConfig::Codex => CODEX_NPM_CACHE,
            _ => return None,
        };
        home.map(|_| expand_home(cache, home))
    }

    /// How to start it. `home` is the user's home folder, for `~/` and for
    /// the state folders; `None` where there is no such thing.
    pub(crate) fn launch(&self, home: Option<&Path>) -> Result<Launch, String> {
        let in_home = |relative: &str| home.map(|home| home.join(relative));
        let expand = |path: &str| expand_home(path, home);

        let launch = match self {
            AgentConfig::ClaudeCode => Launch {
                program: "npx".to_string(),
                args: vec![
                    "-y".to_string(),
                    "--cache".to_string(),
                    expand(CLAUDE_NPM_CACHE).to_string_lossy().into_owned(),
                    CLAUDE_ADAPTER.to_string(),
                ],
                state_dirs: [".npm", ".claude", ".config/claude", "Library/Keychains"]
                    .into_iter()
                    .filter_map(in_home)
                    .collect(),
                state_files: [".claude.json"].into_iter().filter_map(in_home).collect(),
                read_dirs: Vec::new(),
                home_readable: false,
            },
            AgentConfig::Codex => Launch {
                program: "npx".to_string(),
                args: vec![
                    "-y".to_string(),
                    "--cache".to_string(),
                    expand(CODEX_NPM_CACHE).to_string_lossy().into_owned(),
                    CODEX_ADAPTER.to_string(),
                ],
                state_dirs: [".npm", ".codex"].into_iter().filter_map(in_home).collect(),
                state_files: Vec::new(),
                read_dirs: Vec::new(),
                home_readable: false,
            },
            AgentConfig::OpenCode => Launch {
                program: "opencode".to_string(),
                args: vec!["acp".to_string()],
                state_dirs: [
                    ".local/share/opencode",
                    ".local/state/opencode",
                    ".config/opencode",
                    ".cache/opencode",
                ]
                .into_iter()
                .filter_map(in_home)
                .collect(),
                state_files: Vec::new(),
                // OpenCode discovers skills and settings from the folders at
                // the top of home that the other agents keep theirs in, and
                // it reads them, not just lists them. A fence that refuses
                // those reads does not make it fall back to nuza's own
                // settings: OpenCode fails the whole session with "Internal
                // service failure". So the two folders are readable, and
                // only readable - neither is writable, and the vault is
                // still out of reach, since notes reach the agent inside the
                // prompt and nowhere else.
                read_dirs: [".claude", ".agents"]
                    .into_iter()
                    .filter_map(in_home)
                    .collect(),
                home_readable: true,
            },
            AgentConfig::Custom {
                command,
                args,
                data_dirs,
            } => {
                let command = command.trim();
                if command.is_empty() {
                    return Err("could not start the agent: no command was given".to_string());
                }
                let args: Vec<String> = args
                    .iter()
                    .map(|arg| expand(arg).to_string_lossy().into_owned())
                    .collect();
                // An argument that is itself a file (`node ~/agents/mine.js`)
                // is part of the program, and its folder may be read - unless
                // that folder is home itself, which would unlock all of it.
                let read_dirs = args
                    .iter()
                    .map(PathBuf::from)
                    .filter(|path| path.is_file())
                    .filter_map(|path| path.parent().map(Path::to_path_buf))
                    .filter(|dir| Some(dir.as_path()) != home)
                    .collect();
                Launch {
                    program: expand(command).to_string_lossy().into_owned(),
                    args,
                    state_dirs: data_dirs
                        .iter()
                        .map(|dir| dir.trim())
                        .filter(|dir| !dir.is_empty())
                        .map(expand)
                        // Never home itself, whatever was typed: that is
                        // every note and every key on the machine.
                        .filter(|dir| Some(dir.as_path()) != home)
                        .collect(),
                    state_files: Vec::new(),
                    read_dirs,
                    home_readable: false,
                }
            }
        };
        Ok(launch)
    }
}

/// `path` with a leading `~/` put in the home folder.
pub(crate) fn expand_home(path: &str, home: Option<&Path>) -> PathBuf {
    match (path.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => home.join(rest),
        _ if path == "~" => home.map_or_else(|| PathBuf::from(path), Path::to_path_buf),
        _ => PathBuf::from(path),
    }
}

/// The user's home folder.
pub(crate) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> Option<&'static Path> {
        Some(Path::new("/Users/me"))
    }

    #[test]
    fn a_preset_is_started_from_the_pinned_adapter() {
        let claude = AgentConfig::ClaudeCode.launch(home()).unwrap();
        assert_eq!(claude.program, "npx");
        let cache = Path::new("/Users/me").join(".npm/_nuza-claude");
        assert_eq!(
            claude.args,
            ["-y", "--cache", &*cache.to_string_lossy(), CLAUDE_ADAPTER]
        );
        assert!(claude
            .state_dirs
            .contains(&PathBuf::from("/Users/me/.claude")));
        assert_eq!(
            claude.state_files,
            [PathBuf::from("/Users/me/.claude.json")]
        );

        let codex = AgentConfig::Codex.launch(home()).unwrap();
        let cache = Path::new("/Users/me").join(".npm/_nuza-codex");
        assert_eq!(
            codex.args,
            ["-y", "--cache", &*cache.to_string_lossy(), CODEX_ADAPTER]
        );
        assert_eq!(AgentConfig::Codex.npm_cache(home()), Some(cache));
        assert_eq!(AgentConfig::OpenCode.npm_cache(home()), None);

        let opencode = AgentConfig::OpenCode.launch(home()).unwrap();
        assert_eq!(
            (opencode.program.as_str(), opencode.args.as_slice()),
            ("opencode", &["acp".to_string()][..])
        );
        // OpenCode reads the other agents' folders at the top of home while
        // it starts, so the fence has to let it read them.
        assert_eq!(
            opencode.read_dirs,
            [
                PathBuf::from("/Users/me/.claude"),
                PathBuf::from("/Users/me/.agents"),
            ]
        );
        // It also resolves the home folder itself while it starts, and
        // refuses to run when that read is refused - so it needs home's
        // listing, which the other agents do not.
        assert!(opencode.home_readable);
        assert!(!claude.home_readable && !codex.home_readable);
    }

    #[test]
    fn pins_are_exact_versions() {
        for pin in [CLAUDE_ADAPTER, CODEX_ADAPTER] {
            let version = pin.rsplit_once('@').unwrap().1;
            assert!(
                version.split('.').count() == 3
                    && version.chars().all(|c| c.is_ascii_digit() || c == '.'),
                "{pin} is not pinned to an exact version"
            );
        }
    }

    #[test]
    fn the_pins_shown_in_settings_are_the_ones_that_run() {
        let shown = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/lib/ai/agents.ts"),
        )
        .expect("the frontend lists the agents");
        for pin in [CLAUDE_ADAPTER, CODEX_ADAPTER] {
            assert!(
                shown.contains(pin),
                "src/lib/ai/agents.ts does not show {pin}"
            );
        }
    }

    #[test]
    fn a_custom_agent_reads_its_own_folders_and_writes_only_what_it_listed() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("agent.js");
        std::fs::write(&script, "").unwrap();
        let custom = AgentConfig::Custom {
            command: "node".into(),
            args: vec![script.to_string_lossy().into_owned(), "--acp".into()],
            data_dirs: vec!["~/.my-agent".into(), "  ".into(), "~".into()],
        };
        let launch = custom.launch(home()).unwrap();
        assert_eq!(launch.read_dirs, [dir.path().to_path_buf()]);
        // Home itself is never handed over, however it was typed.
        assert_eq!(launch.state_dirs, [PathBuf::from("/Users/me/.my-agent")]);
    }

    #[test]
    fn a_script_in_home_itself_does_not_unlock_home() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("agent.js"), "").unwrap();
        let custom = AgentConfig::Custom {
            command: "node".into(),
            args: vec!["~/agent.js".into()],
            data_dirs: vec![],
        };
        let launch = custom.launch(Some(home.path())).unwrap();
        assert_eq!(
            launch.args,
            [home.path().join("agent.js").to_string_lossy()]
        );
        assert!(launch.read_dirs.is_empty());
    }

    #[test]
    fn an_empty_command_is_refused() {
        let custom = AgentConfig::Custom {
            command: "  ".into(),
            args: vec![],
            data_dirs: vec![],
        };
        assert!(custom.launch(home()).is_err());
    }

    #[test]
    fn every_agent_is_called_by_its_own_name() {
        assert_eq!(AgentConfig::ClaudeCode.label(), "Claude Code");
        assert_eq!(AgentConfig::OpenCode.label(), "OpenCode");
        let custom = AgentConfig::Custom {
            command: "/opt/bin/my-agent".into(),
            args: vec![],
            data_dirs: vec![],
        };
        assert_eq!(custom.label(), "my-agent");
        let script = AgentConfig::Custom {
            command: "node".into(),
            args: vec!["~/agents/mock-acp-agent.mjs".into(), "--flavor".into()],
            data_dirs: vec![],
        };
        assert_eq!(script.label(), "mock-acp-agent");
    }

    #[test]
    fn a_changed_custom_command_is_a_different_agent() {
        let one = AgentConfig::Custom {
            command: "node".into(),
            args: vec!["a.js".into()],
            data_dirs: vec![],
        };
        let two = AgentConfig::Custom {
            command: "node".into(),
            args: vec!["b.js".into()],
            data_dirs: vec![],
        };
        assert_ne!(one.id(), two.id());
        assert_eq!(AgentConfig::Codex.id(), "codex");
    }

    #[test]
    fn the_config_is_stored_by_kind() {
        let json = serde_json::to_value(AgentConfig::ClaudeCode).unwrap();
        assert_eq!(json, serde_json::json!({ "kind": "claudeCode" }));
        let custom: AgentConfig = serde_json::from_value(serde_json::json!({
            "kind": "custom", "command": "x", "args": [], "dataDirs": ["~/.x"]
        }))
        .unwrap();
        assert!(matches!(custom, AgentConfig::Custom { data_dirs, .. } if data_dirs == ["~/.x"]));
    }
}
