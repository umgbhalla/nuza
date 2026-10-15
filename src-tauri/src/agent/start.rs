//! Turning an agent's config into the command that starts it, fenced in
//! where this computer can fence it in.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use tokio::process::Command;

use super::config::{home_dir, AgentConfig, Launch};
use super::status::resolve_command;

/// The one folder an agent may write freely, outside every vault and every
/// session directory: its `TMPDIR`, and its working directory.
const SCRATCH: &str = "nuza-agent-scratch";
/// Where each session's empty working directory is made.
const SESSIONS: &str = "nuza-agent-sessions";

/// Whether this computer runs the agent inside an operating-system sandbox.
/// Where it does not, the agent has the person's own permissions, and the
/// consent dialog says so in as many words.
pub(crate) fn sandboxed() -> bool {
    #[cfg(target_os = "macos")]
    {
        Path::new(SANDBOX_EXEC).exists()
    }
    #[cfg(target_os = "linux")]
    {
        super::confine::abi().is_some()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        false
    }
}

/// Why this computer cannot fence the agent in, when it cannot.
pub(crate) fn unfenced_because() -> Option<&'static str> {
    if sandboxed() {
        return None;
    }
    Some(if cfg!(target_os = "macos") {
        "macOS's sandbox-exec is missing from this Mac."
    } else if cfg!(target_os = "linux") {
        "This Linux kernel has no Landlock, which nuza uses to fence agents in (it needs Linux 5.13 or newer, with Landlock turned on)."
    } else {
        "nuza has no way to fence a program in on Windows."
    })
}

#[cfg(target_os = "macos")]
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// The temporary folder, as the sandbox sees it: resolved on macOS, where it
/// is behind a link and Seatbelt matches real paths. Left as it is elsewhere -
/// resolving it on Windows gives a `\\?\` path agents are not expecting.
fn temp_root() -> PathBuf {
    let temp = std::env::temp_dir();
    if cfg!(target_os = "macos") {
        temp.canonicalize().unwrap_or(temp)
    } else {
        temp
    }
}

/// Where the sessions' working directories go.
pub(crate) fn sessions_root() -> PathBuf {
    temp_root().join(SESSIONS)
}

/// The environment changes a program started from inside an AppImage needs,
/// given the variables nuza was started with and the AppImage's mount point
/// (`APPDIR`): every entry pointing into the AppImage taken out, and a
/// variable left with nothing removed. The AppImage puts its own libraries
/// first in `LD_LIBRARY_PATH` and its own GTK modules in others; a system
/// `node` or agent that inherits them loads libraries it was not built
/// against, and can fail to start at all.
pub(crate) fn outside_appimage(
    vars: impl IntoIterator<Item = (String, String)>,
    appdir: &str,
) -> Vec<(String, Option<String>)> {
    let appdir = appdir.trim_end_matches('/');
    if appdir.is_empty() {
        return Vec::new();
    }
    let inside = |entry: &str| entry == appdir || entry.starts_with(&format!("{appdir}/"));
    let mut changes = Vec::new();
    for (key, value) in vars {
        if key == "NUZA_APPIMAGE_RESTARTED" {
            changes.push((key, None));
            continue;
        }
        if !value.contains(appdir) {
            continue;
        }
        let kept: Vec<&str> = value.split(':').filter(|entry| !inside(entry)).collect();
        let kept = kept.join(":");
        if kept == value {
            continue;
        }
        changes.push((key, (!kept.is_empty()).then_some(kept)));
    }
    changes
}

/// Applies [`outside_appimage`] to `command`, when nuza is running from one.
pub(crate) fn leave_appimage(command: &mut Command) {
    for (key, value) in appimage_changes() {
        match value {
            Some(value) => command.env(key, value),
            None => command.env_remove(key),
        };
    }
}

/// The same, for a standard library command.
pub(crate) fn leave_appimage_std(command: &mut std::process::Command) {
    for (key, value) in appimage_changes() {
        match value {
            Some(value) => command.env(key, value),
            None => command.env_remove(key),
        };
    }
}

fn appimage_changes() -> Vec<(String, Option<String>)> {
    if !cfg!(target_os = "linux") || std::env::var_os("APPIMAGE").is_none() {
        return Vec::new();
    }
    let Some(appdir) = std::env::var("APPDIR").ok() else {
        return Vec::new();
    };
    outside_appimage(std::env::vars(), &appdir)
}

/// Windows gives every console program started from a windowed app a
/// console window of its own, which flashes up and stays while it runs.
/// Agents, `npx` and `--version` checks are all console programs.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Starts `command` without a console window of its own, on Windows.
pub(crate) fn no_window(command: &mut Command) {
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    #[cfg(not(windows))]
    let _ = command;
}

/// The same, for a standard library command.
pub(crate) fn no_window_std(command: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

/// The command that starts `config`'s agent.
///
/// `fence` holds it in with a sandbox. It is off only for full access, when
/// the person has asked for the agent to have the machine the way they do.
pub(crate) fn command_for(config: &AgentConfig, fence: bool) -> Result<Command, String> {
    let path = crate::shellpath::path();
    let home = home_dir();
    let launch = config.launch(home.as_deref())?;
    let name = config.label();

    let program = resolve_command(&launch.program, &path).ok_or_else(|| {
        if config.needs_node() {
            format!("could not start {name}: npx was not found")
        } else {
            format!("could not start {name}: `{}` was not found", launch.program)
        }
    })?;
    if let Some(cache) = config.npm_cache(home.as_deref()) {
        repair_npm_cache(&cache);
    }
    let scratch = temp_root().join(SCRATCH);
    std::fs::create_dir_all(&scratch)
        .map_err(|error| format!("could not start {name}: no scratch folder: {error}"))?;

    let mut command = if fence {
        fenced(&program, &launch, home.as_deref(), &path, &scratch)
            .map_err(|error| format!("could not start {name}: {error}"))?
    } else {
        log::info!("full access is on: {name} starts without a fence");
        unfenced(&program, &launch)
    };
    no_window(&mut command);
    leave_appimage(&mut command);
    command
        .env("PATH", &path)
        .env("TMPDIR", &scratch)
        // Never the app's own working directory, and somewhere the agent can
        // read: Node gives up outright in a working directory it cannot.
        .current_dir(&scratch);
    Ok(command)
}

/// Clears away any adapter download in nuza's own npm cache that was stopped
/// part way - by the app quitting, or the agent being turned off, while `npx`
/// was still fetching it.
///
/// `npx` keeps each package it runs in `_npx/<hash>`, and takes a folder that
/// is there as one that is ready: a half-written one, with no `package.json`
/// yet, fails every start after it with "ENOENT" until it is removed by hand.
/// The cache is nuza's alone, so nothing else can be using what is removed.
pub(crate) fn repair_npm_cache(cache: &Path) {
    let Ok(entries) = std::fs::read_dir(cache.join("_npx")) else {
        return;
    };
    for entry in entries.flatten() {
        let folder = entry.path();
        if folder.is_dir() && !folder.join("package.json").is_file() {
            log::warn!(
                "removing an adapter download that never finished: {}",
                folder.display()
            );
            let _ = std::fs::remove_dir_all(&folder);
        }
    }
}

/// What the agent may read and write, for `program` started as `launch`:
/// the policy both fences are built from.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn fence_for(
    program: &Path,
    launch: &Launch,
    home: Option<&Path>,
    path: &OsStr,
    scratch: &Path,
) -> super::sandbox::Fence {
    use super::sandbox::{node_roots, program_dirs, Fence};

    // Each path as it is given, and as it really is when that is elsewhere:
    // both fences match real paths.
    let real = |paths: &[PathBuf]| -> Vec<PathBuf> {
        let mut all = Vec::new();
        for path in paths {
            all.push(path.clone());
            if let Ok(resolved) = path.canonicalize() {
                if &resolved != path {
                    all.push(resolved);
                }
            }
        }
        all
    };
    let mut read_dirs = launch.read_dirs.clone();
    if let Some(home) = home {
        read_dirs.extend(node_roots(home, path));
        let name = program
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        read_dirs.extend(program_dirs(home, &name, path));
        for dir in [
            program.parent(),
            program
                .canonicalize()
                .ok()
                .as_deref()
                .and_then(Path::parent),
        ]
        .into_iter()
        .flatten()
        {
            if dir.starts_with(home) && dir != home {
                read_dirs.push(dir.to_path_buf());
            }
        }
    }
    // On Linux nothing outside the system's folders is readable unless it is
    // opened up: so also where the program and Node are installed, wherever
    // that is (Homebrew on Linux, for one, is in /home/linuxbrew). Never a
    // folder home is inside.
    if cfg!(target_os = "linux") {
        let node = resolve_command("node", path);
        for found in [Some(program.to_path_buf()), node].into_iter().flatten() {
            for binary in [found.clone(), found.canonicalize().unwrap_or(found)] {
                if let Some(root) = install_root(&binary) {
                    if !home.is_some_and(|home| home.starts_with(&root)) {
                        read_dirs.push(root);
                    }
                }
            }
        }
    }
    Fence {
        home: home.map(|home| home.canonicalize().unwrap_or_else(|_| home.to_path_buf())),
        state_dirs: real(&launch.state_dirs),
        state_files: real(&launch.state_files),
        read_dirs: real(&read_dirs),
        home_readable: launch.home_readable,
        scratch: Some(scratch.to_path_buf()),
    }
}

/// The folder a program is installed in: the one above its `bin`, or its own.
/// Never the root.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn install_root(binary: &Path) -> Option<PathBuf> {
    let dir = binary.parent()?;
    let root = if dir.file_name().is_some_and(|name| name == "bin") {
        dir.parent()?
    } else {
        dir
    };
    (root.parent().is_some()).then(|| root.to_path_buf())
}

#[cfg(target_os = "macos")]
fn fenced(
    program: &Path,
    launch: &Launch,
    home: Option<&Path>,
    path: &OsStr,
    scratch: &Path,
) -> Result<Command, String> {
    use super::sandbox::profile;

    if !sandboxed() {
        log::warn!("there is no sandbox-exec on this Mac; the agent runs without a fence");
        return Ok(unfenced(program, launch));
    }
    let fence = fence_for(program, launch, home, path, scratch);
    let mut command = Command::new(SANDBOX_EXEC);
    command
        .arg("-p")
        .arg(profile(&fence))
        .arg("--")
        .arg(program)
        .args(&launch.args);
    Ok(command)
}

#[cfg(target_os = "linux")]
fn fenced(
    program: &Path,
    launch: &Launch,
    home: Option<&Path>,
    path: &OsStr,
    scratch: &Path,
) -> Result<Command, String> {
    use super::confine::{abi, confine, rules, system_reads, Ruleset};

    let Some(version) = abi() else {
        log::warn!("this kernel has no Landlock; the agent runs with the user's permissions");
        return Ok(unfenced(program, launch));
    };
    let fence = fence_for(program, launch, home, path, scratch);
    let rules = rules(&fence, &system_reads(&temp_root()));
    // The consent dialog said it would be fenced in: if the fence cannot be
    // built, it does not start.
    let ruleset =
        Ruleset::build(&rules).map_err(|error| format!("could not fence the agent in: {error}"))?;
    log::info!("fencing the agent in with Landlock v{version}");
    let mut command = unfenced(program, launch);
    confine(&mut command, ruleset);
    Ok(command)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn fenced(
    program: &Path,
    launch: &Launch,
    _: Option<&Path>,
    _: &OsStr,
    _: &Path,
) -> Result<Command, String> {
    log::warn!("no sandbox for the agent on this system; it runs with the user's permissions");
    Ok(unfenced(program, launch))
}

fn unfenced(program: &Path, launch: &Launch) -> Command {
    let mut command = Command::new(program);
    command.args(&launch.args);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_started_from_an_appimage_does_not_inherit_its_libraries() {
        let vars = [
            (
                "LD_LIBRARY_PATH",
                "/tmp/.mount_nuzaX/usr/lib:/usr/local/lib",
            ),
            ("GIO_MODULE_DIR", "/tmp/.mount_nuzaX/usr/lib/gio/modules"),
            ("XDG_DATA_DIRS", "/tmp/.mount_nuzaX/usr/share:/usr/share"),
            ("PATH", "/usr/bin:/bin"),
            ("NUZA_APPIMAGE_RESTARTED", "1"),
            // Not inside it, only named like it.
            ("NOTES", "/tmp/.mount_nuzaXY/notes"),
        ]
        .map(|(key, value)| (key.to_string(), value.to_string()));
        let changes = outside_appimage(vars, "/tmp/.mount_nuzaX/");
        let change = |key: &str| {
            changes
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(
            change("LD_LIBRARY_PATH"),
            Some(Some("/usr/local/lib".into()))
        );
        assert_eq!(change("GIO_MODULE_DIR"), Some(None));
        assert_eq!(change("XDG_DATA_DIRS"), Some(Some("/usr/share".into())));
        assert_eq!(change("NUZA_APPIMAGE_RESTARTED"), Some(None));
        assert_eq!(change("PATH"), None);
        assert_eq!(change("NOTES"), None);
    }

    #[test]
    fn a_download_stopped_part_way_is_cleared_and_a_finished_one_kept() {
        let cache = tempfile::tempdir().unwrap();
        let npx = cache.path().join("_npx");
        let finished = npx.join("aaaa");
        let half = npx.join("bbbb");
        std::fs::create_dir_all(finished.join("node_modules")).unwrap();
        std::fs::write(finished.join("package.json"), "{}").unwrap();
        std::fs::create_dir_all(half.join("node_modules/@scope/pkg")).unwrap();

        repair_npm_cache(cache.path());

        assert!(finished.join("package.json").is_file());
        assert!(!half.exists());
        // A cache with nothing in it yet is fine too.
        repair_npm_cache(&cache.path().join("nowhere"));
    }
}
