//! nuza's vault tools, as the harness hands them to a session: the
//! read-only server in `vaultserver.rs`, started the first time a session
//! wants it - a nuza that never opens the AI panel never opens a port - and
//! stopped when AI is turned off, so that leaves no port open either.

use std::path::Path;
use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Manager};

use super::harness::{ToolAccess, VaultTools};
use super::store::Store;
use crate::state::{locked, Windows};
use crate::vaultserver::VaultServer;

pub(crate) struct AppVaultTools {
    app: AppHandle,
    /// Started when first wanted, and stopped when AI is turned off.
    server: Mutex<Option<Arc<VaultServer>>>,
}

impl AppVaultTools {
    pub(crate) fn new(app: AppHandle) -> Self {
        Self {
            app,
            server: Mutex::new(None),
        }
    }

    /// The server, if it is running.
    pub(crate) fn running(&self) -> Option<Arc<VaultServer>> {
        locked(&self.server).clone()
    }

    /// Stops the server, if it is running. The next session that wants the
    /// tools starts it again.
    pub(crate) fn stop(&self) {
        if let Some(server) = locked(&self.server).take() {
            server.stop();
        }
    }

    /// The server, started if it is not running.
    fn server(&self) -> Option<Arc<VaultServer>> {
        let mut server = locked(&self.server);
        if server.is_none() {
            match VaultServer::start() {
                Ok(started) => {
                    // Read-only until the person says otherwise, which is what
                    // the store holds. A server started later reads it then.
                    started.set_writes(self.app.state::<Store>().full_access());
                    *server = Some(Arc::new(started));
                }
                Err(error) => log::error!("the vault tools could not start: {error}"),
            }
        }
        server.clone()
    }

    /// Lets the running vault server change notes, or stops it, as the person
    /// turns full access on and off. Nothing to do when it is not running:
    /// the next one reads the store as it starts.
    pub(crate) fn set_writes(&self, allowed: bool) {
        if let Some(server) = self.running() {
            server.set_writes(allowed);
        }
    }
}

impl VaultTools for AppVaultTools {
    fn open(&self, window: &str, vault: &Path) -> Option<ToolAccess> {
        if !self.app.state::<Store>().vault_search(vault) {
            return None;
        }
        // The window's own vault, and still the one asked about.
        let open = self.app.state::<Windows>().vault(window);
        if locked(&open.root).as_deref() != Some(vault) {
            return None;
        }
        let server = self.server()?;
        let token = server.grant(open)?;
        Some(ToolAccess {
            url: server.url().to_string(),
            token,
        })
    }

    fn close(&self, token: &str) {
        if let Some(server) = self.running() {
            server.revoke(token);
        }
    }
}
