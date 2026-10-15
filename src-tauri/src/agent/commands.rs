//! The agent, as the webview can reach it.
//!
//! The webview is not trusted here any more than anywhere else. It names an
//! agent from Settings; it never hands over a program to run, a working
//! directory, a sandbox rule or another window's vault. The window a command
//! came from is Tauri's word, not the webview's, and its vault is looked up
//! from that. And the two commands that widen what can happen to a note -
//! starting an agent nobody has agreed to, letting it onto the web - ask the
//! person first, in a native dialog the webview cannot answer for them.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{Manager, State, WebviewWindow};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

use super::config::AgentConfig;
use super::errors::AgentError;
use super::events::AgentEvent;
use super::harness::{AgentHarness, Emit, ModelInfo, Question};
use super::status::{self, Detection};
use super::store::Store;
use crate::state::Windows;
use crate::tasks::wait_for_picker;

/// Where the AI settings stand, for the window asking.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentState {
    pub(crate) agent: Option<AgentConfig>,
    /// What to call it: "Claude Code", or a custom program's name.
    pub(crate) label: Option<String>,
    /// Whether this computer fences the agent in.
    pub(crate) sandboxed: bool,
    /// Why it does not, where that is worth saying.
    pub(crate) unfenced_because: Option<&'static str>,
    /// Whether the agent may look things up on the web for this window's
    /// vault.
    pub(crate) web_lookups: bool,
    /// Whether this window has a vault open at all.
    pub(crate) has_vault: bool,
    /// Whether the agent may search this window's vault with nuza's tools.
    pub(crate) vault_search: bool,
    /// Whether the agent may do anything it asks to do - run commands, change
    /// and delete notes, fetch pages. Not tied to a window's vault.
    pub(crate) full_access: bool,
    /// Whether the running agent can take the tools; `None` before it has said.
    pub(crate) agent_searches: Option<bool>,
}

fn vault_of(window: &WebviewWindow) -> Option<PathBuf> {
    window.state::<Windows>().root(window.label())
}

async fn merged_path() -> std::ffi::OsString {
    tauri::async_runtime::spawn_blocking(crate::shellpath::path)
        .await
        .unwrap_or_else(|_| std::env::var_os("PATH").unwrap_or_default())
}

/// Asks the person, natively, and waits for the answer.
async fn confirm(window: &WebviewWindow, title: String, body: String, yes: &str) -> bool {
    let yes = yes.to_string();
    wait_for_picker(|send| {
        window
            .dialog()
            .message(body)
            .title(title)
            .kind(MessageDialogKind::Info)
            .buttons(MessageDialogButtons::OkCancelCustom(
                yes,
                "Cancel".to_string(),
            ))
            .parent(window)
            .show(move |agreed| send(Ok(agreed)));
    })
    .await
    .unwrap_or(false)
}

/// The one-time notice in front of starting an agent. The line about the
/// sandbox is the honest one for this computer, and is not softened where
/// the answer is no.
pub(crate) fn consent_notice(config: &AgentConfig, sandboxed: bool) -> (String, String) {
    let name = config.label();
    let fence = if sandboxed {
        "It runs fenced in: it sees only what nuza sends it, and cannot change your notes or any of your files. Anything beyond that is refused."
    } else {
        "This computer can't fence it in, so it runs with your own permissions."
    };
    let mut body = format!(
        "nuza starts it on this computer and sends it your questions, the text you select, and the note text it needs to answer.\n\n\
         It is not made by nuza. Its sign-in, its usage limits and what it does with your data are set by its own provider.\n\n\
         {fence}"
    );
    if let AgentConfig::Custom { command, args, .. } = config {
        let line = std::iter::once(command.trim())
            .chain(args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        body.push_str(&format!("\n\nIt will run:\n{line}"));
    }
    (format!("Turn on {name}?"), body)
}

#[tauri::command]
pub(crate) async fn agent_state(
    window: WebviewWindow,
    harness: State<'_, Arc<AgentHarness>>,
    store: State<'_, Store>,
) -> Result<AgentState, String> {
    let agent = harness.current();
    let vault = vault_of(&window);
    Ok(AgentState {
        label: agent.as_ref().map(AgentConfig::label),
        agent,
        sandboxed: super::start::sandboxed(),
        unfenced_because: super::start::unfenced_because(),
        web_lookups: vault
            .as_deref()
            .is_some_and(|vault| store.web_lookups(vault)),
        has_vault: vault.is_some(),
        vault_search: vault
            .as_deref()
            .is_some_and(|vault| store.vault_search(vault)),
        full_access: store.full_access(),
        agent_searches: harness.takes_tools().await,
    })
}

/// What is installed. Runs local programs only, and contacts nothing.
#[tauri::command]
pub(crate) async fn agent_detect() -> Detection {
    status::detect(&merged_path().await).await
}

/// Where a custom agent's command is, if it is anywhere.
#[tauri::command]
pub(crate) async fn agent_find_command(command: String) -> Option<String> {
    status::resolve_command(&command, &merged_path().await)
        .map(|at| at.to_string_lossy().into_owned())
}

/// Turns `config`'s agent on and starts it. Returns false if the person
/// would rather not after all.
#[tauri::command]
pub(crate) async fn agent_use(
    window: WebviewWindow,
    config: AgentConfig,
    harness: State<'_, Arc<AgentHarness>>,
    store: State<'_, Store>,
) -> Result<bool, AgentError> {
    if !store.approved(&config) {
        let (title, body) = consent_notice(&config, super::start::sandboxed());
        if !confirm(&window, title, body, "Turn On").await {
            return Ok(false);
        }
        store.update(|decisions| decisions.approved.push(config.id()));
    }
    store.update(|decisions| decisions.agent = Some(config.clone()));
    harness.use_agent(config).await?;
    Ok(true)
}

/// Turns AI off: the agent is stopped, the vault tools stop listening, and
/// nothing is started again until an agent is turned on.
#[tauri::command]
pub(crate) async fn agent_turn_off(
    harness: State<'_, Arc<AgentHarness>>,
    tools: State<'_, Arc<super::tools::AppVaultTools>>,
    store: State<'_, Store>,
) -> Result<(), String> {
    store.update(|decisions| decisions.agent = None);
    harness.set_agent(None).await;
    tools.stop();
    Ok(())
}

/// AI turned off or on in Settings. Off stops the agent and the vault tools -
/// no process, no port - but keeps which agent was chosen, so turning AI on
/// again picks up where it was without asking again.
#[tauri::command]
pub(crate) async fn agent_set_enabled(
    enabled: bool,
    harness: State<'_, Arc<AgentHarness>>,
    tools: State<'_, Arc<super::tools::AppVaultTools>>,
    store: State<'_, Store>,
) -> Result<(), String> {
    if enabled {
        harness.restore(store.get().agent);
    } else {
        harness.set_agent(None).await;
        tools.stop();
    }
    Ok(())
}

/// Starts the agent ahead of the first question.
#[tauri::command]
pub(crate) async fn agent_warm(harness: State<'_, Arc<AgentHarness>>) -> Result<(), AgentError> {
    harness.warm().await
}

#[tauri::command]
pub(crate) async fn agent_list_models(
    harness: State<'_, Arc<AgentHarness>>,
) -> Result<Vec<ModelInfo>, AgentError> {
    harness.list_models().await
}

/// Asks the agent something in this window's conversation. What it says
/// arrives on `on_event`, ending with `done` or `failed`. `in_full` is the
/// same question with the note and the rules, for when the agent turns out
/// not to have the conversation - a new one, or one it lost.
#[tauri::command]
pub(crate) async fn agent_prompt(
    window: WebviewWindow,
    text: String,
    in_full: Option<String>,
    note_path: Option<String>,
    model: Option<String>,
    on_event: Channel<AgentEvent>,
    harness: State<'_, Arc<AgentHarness>>,
) -> Result<(), String> {
    let emit: Emit = Arc::new(move |event| {
        let _ = on_event.send(event);
    });
    let vault = vault_of(&window);
    // The note's path in the vault, if it is one - checked here, against the
    // window's own vault, before the agent is told it.
    let note = note_path.and_then(|path| in_vault(&window, &path));
    // The outcome has gone down the channel already, as the turn's last event.
    let _ = harness
        .prompt(
            window.label(),
            vault.as_deref(),
            Question {
                text: &text,
                in_full: in_full.as_deref(),
                note: note.as_deref(),
            },
            model.as_deref(),
            emit,
        )
        .await;
    Ok(())
}

#[tauri::command]
pub(crate) async fn agent_cancel(
    window: WebviewWindow,
    harness: State<'_, Arc<AgentHarness>>,
) -> Result<(), String> {
    harness.cancel(window.label()).await
}

#[tauri::command]
pub(crate) async fn agent_new_chat(
    window: WebviewWindow,
    harness: State<'_, Arc<AgentHarness>>,
) -> Result<(), String> {
    harness.new_chat(window.label()).await;
    Ok(())
}

/// `path`, relative to the window's vault and with forward slashes, if it is
/// a file inside it.
fn in_vault(window: &WebviewWindow, path: &str) -> Option<String> {
    let open = crate::state::vault_of(window);
    let found = crate::state::within_vault(&open, std::path::Path::new(path)).ok()?;
    let root = crate::state::locked(&open.root).clone()?;
    let relative = found.strip_prefix(&root).ok()?;
    Some(
        relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// Lets the agent search and read this window's vault with nuza's tools, or
/// stops it - for conversations started from now on.
#[tauri::command]
pub(crate) async fn agent_set_vault_search(
    window: WebviewWindow,
    allowed: bool,
    harness: State<'_, Arc<AgentHarness>>,
    store: State<'_, Store>,
) -> Result<(), String> {
    let vault = vault_of(&window).ok_or("Open a folder first")?;
    store.update(|decisions| {
        decisions.no_search_vaults.retain(|kept| kept != &vault);
        if !allowed {
            decisions.no_search_vaults.push(vault.clone());
        }
    });
    // The conversation under way was started with or without the tools; the
    // next one is started the new way.
    harness.new_chat(window.label()).await;
    Ok(())
}

/// The notes this window's agent has been shown, which are the only ones a
/// citation in its answer may link to.
#[tauri::command]
pub(crate) async fn agent_citable(
    window: WebviewWindow,
    harness: State<'_, Arc<AgentHarness>>,
    tools: State<'_, Arc<super::tools::AppVaultTools>>,
) -> Result<Vec<String>, String> {
    let (Some(token), Some(server)) = (harness.tools_token(window.label()).await, tools.running())
    else {
        return Ok(Vec::new());
    };
    Ok(server.given(&token))
}

/// Where a citation points: a note the agent was shown, and the line its
/// paragraph starts on.
#[derive(Debug, Serialize)]
pub(crate) struct Cited {
    pub(crate) path: String,
    pub(crate) line: usize,
}

/// Finds a citation's note, for the panel to open it at the paragraph cited.
#[tauri::command]
pub(crate) async fn agent_cite(
    window: WebviewWindow,
    path: String,
    paragraph: Option<usize>,
    harness: State<'_, Arc<AgentHarness>>,
    tools: State<'_, Arc<super::tools::AppVaultTools>>,
) -> Result<Cited, String> {
    let not_given = || "That note wasn't one the agent was shown".to_string();
    let token = harness
        .tools_token(window.label())
        .await
        .ok_or_else(not_given)?;
    let server = tools.running().ok_or_else(not_given)?;
    let (path, line) = server
        .locate(&token, &path, paragraph)
        .ok_or_else(not_given)?;
    Ok(Cited { path, line })
}

/// Starts `codex login`, which opens a browser of its own.
#[tauri::command]
pub(crate) async fn agent_login() -> Result<(), String> {
    status::codex_login(&merged_path().await)
}

/// Lets the agent look things up on the web for this window's vault, or
/// stops it. Turning it on is asked about first.
#[tauri::command]
pub(crate) async fn agent_set_web_lookups(
    window: WebviewWindow,
    allowed: bool,
    harness: State<'_, Arc<AgentHarness>>,
    store: State<'_, Store>,
) -> Result<bool, String> {
    let vault = vault_of(&window).ok_or("Open a folder first")?;
    if allowed && !store.web_lookups(&vault) {
        let name = harness
            .current()
            .map_or_else(|| "The agent".to_string(), |agent| agent.label());
        let body = format!(
            "For the notes in this folder, {name} will be allowed to fetch web pages when it asks.\n\n\
             A note can hold text written to talk an agent into things - a pasted article, a file someone shared. \
             With this on, a note like that could have {name} send what it has read to a website.\n\n\
             Leave it off unless you need it."
        );
        let title = "Let the agent look things up on the web?".to_string();
        if !confirm(&window, title, body, "Allow").await {
            return Ok(false);
        }
    }
    store.update(|decisions| {
        decisions.web_vaults.retain(|kept| kept != &vault);
        if allowed {
            decisions.web_vaults.push(vault.clone());
        }
    });
    harness.set_web_lookups(&vault, allowed);
    Ok(allowed)
}

/// Lets the agent do anything it asks to do - run commands, change and delete
/// notes, fetch pages - or stops letting it.
///
/// Not tied to a folder: it covers what the agent does wherever it is turned
/// on, which is why turning it on is asked about first, in a dialog the
/// webview cannot draw or click through.
#[tauri::command]
pub(crate) async fn agent_set_full_access(
    window: WebviewWindow,
    allowed: bool,
    harness: State<'_, Arc<AgentHarness>>,
    store: State<'_, Store>,
) -> Result<bool, String> {
    if allowed && !store.full_access() {
        let name = harness
            .current()
            .map_or_else(|| "The agent".to_string(), |agent| agent.label());
        let body = format!(
            "{name} will be allowed to do whatever it asks to do, everywhere it is turned on.\n\n\
             That includes running commands on this computer, and changing, moving and deleting notes \
             in any folder nuza has shown it. A note can hold text written to talk an agent into things - \
             a pasted article, a file someone shared - and with this on, a note like that could have \
             {name} change other notes, or run a command.\n\n\
             Deleting a note moves it to the trash, so it can be brought back. Everything else cannot.\n\n\
             Leave it off unless you need it."
        );
        let title = "Give the agent full access?".to_string();
        if !confirm(&window, title, body, "Give full access").await {
            return Ok(false);
        }
    }
    store.update(|decisions| decisions.full_access = allowed);
    harness.set_full_access(allowed);
    Ok(allowed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_notice_says_plainly_when_the_agent_is_not_fenced_in() {
        let (title, fenced) = consent_notice(&AgentConfig::ClaudeCode, true);
        assert_eq!(title, "Turn on Claude Code?");
        assert!(fenced.contains("fenced in"));
        let (_, open) = consent_notice(&AgentConfig::ClaudeCode, false);
        assert!(
            open.contains("This computer can't fence it in, so it runs with your own permissions.")
        );
        assert!(!open.contains("fenced in:"));
    }

    #[test]
    fn a_custom_agent_is_agreed_to_with_its_whole_command_in_view() {
        let custom = AgentConfig::Custom {
            command: "node".into(),
            args: vec!["~/agents/mine.js".into(), "--acp".into()],
            data_dirs: vec![],
        };
        let (title, body) = consent_notice(&custom, true);
        assert_eq!(title, "Turn on mine?");
        assert!(
            body.ends_with("It will run:\nnode ~/agents/mine.js --acp"),
            "{body}"
        );
    }
}
