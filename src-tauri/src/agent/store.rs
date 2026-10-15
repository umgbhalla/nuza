//! What the person has decided about the agent, kept where the webview cannot
//! reach it.
//!
//! Which agent to start, which agents they have agreed to have started, and
//! on which vaults the agent may fetch web pages. All three widen what can
//! happen to a note, so none of them is the webview's to say: they live in a
//! file in the app's data folder, written only by commands that first put a
//! dialog in front of the person - one the webview does not draw, and so
//! cannot click through for them.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::config::AgentConfig;
use crate::state::locked;

const STORE_FILE: &str = "agent.json";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct Decisions {
    /// The agent that is turned on, if one is.
    pub(crate) agent: Option<AgentConfig>,
    /// Every agent the person has agreed to, by `AgentConfig::id`. Agreeing
    /// to Claude Code is not agreeing to Codex, and agreeing to one custom
    /// command is not agreeing to another.
    pub(crate) approved: Vec<String>,
    /// Vaults on which the agent may look things up on the web.
    pub(crate) web_vaults: Vec<PathBuf>,
    /// Vaults whose owner turned the vault tools off. They are on everywhere
    /// else - the note text is sent either way, and the tools only read.
    pub(crate) no_search_vaults: Vec<PathBuf>,
    /// Whether the agent may do anything on this machine it can ask to do:
    /// run commands, change and delete notes, look things up. Off - which is
    /// how nuza ships - it may only read the vault, through nuza's own tools.
    pub(crate) full_access: bool,
}

pub(crate) struct Store {
    path: Option<PathBuf>,
    decisions: Mutex<Decisions>,
}

impl Store {
    /// The decisions kept in `folder`, or none yet.
    pub(crate) fn open(folder: Option<&Path>) -> Self {
        let path = folder.map(|folder| folder.join(STORE_FILE));
        let decisions = path
            .as_deref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            path,
            decisions: Mutex::new(decisions),
        }
    }

    pub(crate) fn get(&self) -> Decisions {
        locked(&self.decisions).clone()
    }

    /// Changes the decisions and writes them down.
    pub(crate) fn update(&self, change: impl FnOnce(&mut Decisions)) {
        let mut decisions = locked(&self.decisions);
        change(&mut decisions);
        let Some(path) = &self.path else { return };
        let Ok(json) = serde_json::to_vec_pretty(&*decisions) else {
            return;
        };
        if let Some(folder) = path.parent() {
            let _ = std::fs::create_dir_all(folder);
        }
        if let Err(error) = crate::files::write_atomically(path, &json) {
            log::error!("could not keep the agent settings: {error}");
        }
    }

    pub(crate) fn approved(&self, config: &AgentConfig) -> bool {
        locked(&self.decisions).approved.contains(&config.id())
    }

    /// Whether the agent may search and read `vault` with nuza's tools.
    pub(crate) fn vault_search(&self, vault: &Path) -> bool {
        !locked(&self.decisions)
            .no_search_vaults
            .iter()
            .any(|kept| kept == vault)
    }

    pub(crate) fn web_lookups(&self, vault: &Path) -> bool {
        locked(&self.decisions)
            .web_vaults
            .iter()
            .any(|kept| kept == vault)
    }

    /// Whether the agent may do anything it can ask to do, rather than only
    /// read the vault.
    pub(crate) fn full_access(&self) -> bool {
        locked(&self.decisions).full_access
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decisions_survive_a_restart() {
        let folder = tempfile::tempdir().unwrap();
        let store = Store::open(Some(folder.path()));
        assert_eq!(store.get(), Decisions::default());

        store.update(|decisions| {
            decisions.agent = Some(AgentConfig::ClaudeCode);
            decisions.approved.push(AgentConfig::ClaudeCode.id());
            decisions.web_vaults.push(PathBuf::from("/notes/work"));
        });

        let again = Store::open(Some(folder.path()));
        assert_eq!(again.get().agent, Some(AgentConfig::ClaudeCode));
        assert!(again.approved(&AgentConfig::ClaudeCode));
        assert!(!again.approved(&AgentConfig::Codex));
        assert!(again.web_lookups(Path::new("/notes/work")));
        assert!(!again.web_lookups(Path::new("/notes/home")));
    }

    #[test]
    fn a_file_that_makes_no_sense_is_no_decisions_at_all() {
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(folder.path().join(STORE_FILE), "{ not json").unwrap();
        assert_eq!(Store::open(Some(folder.path())).get(), Decisions::default());
    }
}
