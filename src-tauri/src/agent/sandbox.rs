//! The fence an agent runs inside on macOS: a Seatbelt profile, applied with
//! `sandbox-exec`, around the agent's whole process tree.
//!
//! nuza's promise that the agent cannot change a note cannot rest on the
//! agent's cooperation. An agent's own "read-only" mode is a request, and at
//! least one well-known agent has been found writing outside its session
//! directory while in one. Claude Code's "plan" mode wrote into `~/.claude`
//! without asking (docs/acp-transcript.md). So the operating system holds the
//! line instead:
//!
//! - **Writes are refused everywhere** except the agent's own state - its
//!   sign-in, its npm cache - and a scratch folder nuza gives it as `TMPDIR`.
//!   Not the session's working directory, not the vault, not the home folder.
//! - **The home folder cannot be read**, except that same state, `~/.npmrc`,
//!   the folders the agent's own programs live in when they live in home
//!   (an nvm Node, a script a custom agent runs), and - for an agent that
//!   resolves the home folder itself as it starts - the *listing* of home,
//!   names only, never a byte of what is in it. The vault lives in home, so
//!   the agent cannot read it: notes reach it through the prompt, and nothing
//!   else. Nor `~/.ssh`, nor a browser's profile, nor another vault.
//! - **`/Volumes` cannot be read**, so an external disk is no way around it.
//! - **The network stays open.** No agent can answer without reaching its own
//!   provider. Whether it may fetch a web page as a tool is a separate
//!   question, answered at the permission layer.

use std::path::{Path, PathBuf};

/// What the profile is built from.
#[derive(Debug, Default)]
pub(crate) struct Fence {
    pub(crate) home: Option<PathBuf>,
    /// Folders in home the agent may read and write.
    pub(crate) state_dirs: Vec<PathBuf>,
    /// Files in home the agent may read and write, by prefix.
    pub(crate) state_files: Vec<PathBuf>,
    /// Folders in home the agent may read.
    pub(crate) read_dirs: Vec<PathBuf>,
    /// Whether the agent may read the home folder's own directory entry - its
    /// listing, not the contents of anything inside it.
    pub(crate) home_readable: bool,
    /// Somewhere outside the vault and the session directories the agent may
    /// write freely - its `TMPDIR`.
    pub(crate) scratch: Option<PathBuf>,
}

/// The Seatbelt profile for `fence`, for `sandbox-exec -p`.
pub(crate) fn profile(fence: &Fence) -> String {
    let mut reads = String::new();
    let mut writes = String::new();
    let quote = |path: &Path| escape(&path.to_string_lossy());

    if let Some(home) = &fence.home {
        reads.push_str(&format!(
            "(deny file-read-data (subpath \"{}\"))\n",
            quote(home)
        ));
        let readable = fence
            .state_dirs
            .iter()
            .chain(&fence.read_dirs)
            .filter(|dir| dir.as_path() != home.as_path());
        for dir in readable {
            reads.push_str(&format!(
                "(allow file-read-data (subpath \"{}\"))\n",
                quote(dir)
            ));
        }
        // The listing of home itself, where an agent resolves the home folder
        // as it starts: the names of what is in home, never a byte of it. The
        // denies stay in place for everything below, so nothing in home can
        // actually be read.
        if fence.home_readable {
            reads.push_str(&format!(
                "(allow file-read-data (literal \"{}\"))\n",
                quote(home)
            ));
        }
        reads.push_str(&format!(
            "(allow file-read-data (literal \"{}\"))\n",
            quote(&home.join(".npmrc"))
        ));
    }
    for file in &fence.state_files {
        let prefix = regex_prefix(file);
        reads.push_str(&format!("(allow file-read-data (regex #\"{prefix}\"))\n"));
        writes.push_str(&format!("(allow file-write* (regex #\"{prefix}\"))\n"));
    }
    reads.push_str("(deny file-read-data (subpath \"/Volumes\"))\n");

    let writable = fence
        .state_dirs
        .iter()
        .filter(|dir| Some(dir.as_path()) != fence.home.as_deref())
        .chain(&fence.scratch);
    for dir in writable {
        writes.push_str(&format!(
            "(allow file-write* (subpath \"{}\"))\n",
            quote(dir)
        ));
    }

    format!(
        "(version 1)\n\
         (deny default)\n\
         (allow process-fork)\n\
         (allow process-exec)\n\
         (allow signal)\n\
         (allow sysctl-read)\n\
         (allow mach-lookup)\n\
         (allow ipc-posix-shm)\n\
         (allow system-socket)\n\
         (allow file-read*)\n\
         {reads}\
         (allow file-write-data (literal \"/dev/null\"))\n\
         (allow file-write-data (literal \"/dev/tty\"))\n\
         (allow file-ioctl (literal \"/dev/tty\"))\n\
         {writes}\
         (allow network*)\n"
    )
}

/// The Node installs on `path` that live in `home` - nvm's
/// `~/.nvm/versions/node/v24/bin`, say - as the folder above each `bin`.
/// Installs outside home (Homebrew, the system's) are readable already.
pub(crate) fn node_roots(home: &Path, path: &std::ffi::OsStr) -> Vec<PathBuf> {
    std::env::split_paths(path)
        .filter(|dir| dir.starts_with(home) && dir.as_path() != home)
        .filter(|dir| dir.join("node").exists() || dir.join("npx").exists())
        .filter_map(|dir| dir.parent().map(Path::to_path_buf))
        .filter(|root| root.as_path() != home)
        .collect()
}

/// The folders, in home, that a program called `name` is found in along
/// `path` - every one of them, since a wrapper on `PATH` may hand over to the
/// real program further along - and where each really lives, if that is
/// somewhere else in home. Never home itself.
pub(crate) fn program_dirs(home: &Path, name: &str, path: &std::ffi::OsStr) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for dir in std::env::split_paths(path) {
        let program = dir.join(name);
        if !program.is_file() {
            continue;
        }
        dirs.push(dir.clone());
        if let Some(real) = program
            .canonicalize()
            .ok()
            .and_then(|real| real.parent().map(Path::to_path_buf))
        {
            dirs.push(real);
        }
    }
    dirs.retain(|dir| dir.starts_with(home) && dir.as_path() != home);
    dirs.dedup();
    dirs
}

/// A regular expression matching `path` and anything that starts with it.
fn regex_prefix(path: &Path) -> String {
    let mut pattern = String::from("^");
    for c in path.to_string_lossy().chars() {
        if c.is_ascii_alphanumeric() || c == '/' || c == '_' || c == '-' {
            pattern.push(c);
        } else if c == '"' {
            pattern.push_str("\\\"");
        } else {
            pattern.push('\\');
            pattern.push(c);
        }
    }
    pattern
}

/// A path, for a string in a Seatbelt profile.
fn escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

// Unix paths throughout, as Seatbelt's are.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn fence() -> Fence {
        Fence {
            home: Some(PathBuf::from("/Users/me")),
            state_dirs: vec![
                PathBuf::from("/Users/me/.npm"),
                PathBuf::from("/Users/me/.claude"),
            ],
            state_files: vec![PathBuf::from("/Users/me/.claude.json")],
            read_dirs: vec![PathBuf::from("/Users/me/.nvm/versions/node/v24")],
            scratch: Some(PathBuf::from("/private/tmp/nuza-agent-scratch")),
            home_readable: false,
        }
    }

    #[test]
    fn writes_go_only_to_the_agents_own_state_and_its_scratch() {
        let profile = profile(&fence());
        assert!(profile.contains("(deny default)"));
        assert!(profile.contains("(allow file-write* (subpath \"/Users/me/.npm\"))"));
        assert!(profile.contains("(allow file-write* (subpath \"/Users/me/.claude\"))"));
        assert!(profile.contains("(allow file-write* (regex #\"^/Users/me/\\.claude\\.json\"))"));
        assert!(
            profile.contains("(allow file-write* (subpath \"/private/tmp/nuza-agent-scratch\"))")
        );
        assert!(!profile.contains("file-write* (subpath \"/Users/me\")"));
        // Somewhere to run a program from is not somewhere to write.
        assert!(!profile.contains("file-write* (subpath \"/Users/me/.nvm"));
        assert!(profile.contains("(allow network*)"));
    }

    #[test]
    fn home_is_locked_and_the_exceptions_come_after_the_lock() {
        let profile = profile(&fence());
        let lock = profile
            .find("(deny file-read-data (subpath \"/Users/me\"))")
            .unwrap();
        for exception in [
            "(allow file-read-data (subpath \"/Users/me/.npm\"))",
            "(allow file-read-data (subpath \"/Users/me/.nvm/versions/node/v24\"))",
            "(allow file-read-data (literal \"/Users/me/.npmrc\"))",
        ] {
            let at = profile
                .find(exception)
                .unwrap_or_else(|| panic!("no {exception}"));
            assert!(
                lock < at,
                "{exception} comes before the lock, which then overrides it"
            );
        }
        assert!(profile.contains("(deny file-read-data (subpath \"/Volumes\"))"));
    }

    #[test]
    fn home_itself_is_never_an_exception() {
        let mut fence = fence();
        fence.state_dirs.push(PathBuf::from("/Users/me"));
        fence.read_dirs.push(PathBuf::from("/Users/me"));
        let profile = profile(&fence);
        assert!(!profile.contains("(allow file-read-data (subpath \"/Users/me\"))"));
        assert!(!profile.contains("(allow file-write* (subpath \"/Users/me\"))"));
    }

    #[test]
    fn an_agent_that_resolves_home_sees_its_listing_and_nothing_more() {
        let fence = Fence {
            home_readable: true,
            ..fence()
        };
        let profile = profile(&fence);
        let lock = profile
            .find("(deny file-read-data (subpath \"/Users/me\"))")
            .unwrap();
        let listing = profile
            .find("(allow file-read-data (literal \"/Users/me\"))")
            .unwrap_or_else(|| panic!("no listing rule in {profile}"));
        assert!(lock < listing, "the listing rule must come after the lock");
        // The listing is home's own entry, never a folder inside it.
        assert!(!profile.contains("(allow file-read-data (subpath \"/Users/me\"))"));
    }

    #[test]
    fn quotes_in_a_path_cannot_end_the_rule_early() {
        let fence = Fence {
            home: Some(PathBuf::from(r#"/Users/we"ird"#)),
            state_files: vec![PathBuf::from(r#"/Users/we"ird/.x"#)],
            ..Fence::default()
        };
        let profile = profile(&fence);
        assert!(
            profile.contains(r#"(subpath "/Users/we\"ird")"#),
            "{profile}"
        );
        assert!(profile.contains(r#"#"^/Users/we\"ird/\.x""#), "{profile}");
    }

    #[test]
    fn only_node_installs_inside_home_are_opened_up() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let nvm = home.join(".nvm/versions/node/v24/bin");
        let local = home.join(".local/bin");
        std::fs::create_dir_all(&nvm).unwrap();
        std::fs::create_dir_all(&local).unwrap();
        std::fs::write(nvm.join("node"), "").unwrap();
        let path =
            std::env::join_paths([nvm.as_path(), local.as_path(), Path::new("/usr/bin")]).unwrap();
        assert_eq!(
            node_roots(&home, &path),
            [home.join(".nvm/versions/node/v24")]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_program_is_readable_where_it_lives_and_where_it_really_is() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().canonicalize().unwrap().join("home");
        let bin = home.join(".local/bin");
        let real = home.join(".opencode/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("opencode"), "").unwrap();
        std::os::unix::fs::symlink(real.join("opencode"), bin.join("opencode")).unwrap();
        let path = std::env::join_paths([bin.as_path(), Path::new("/usr/bin")]).unwrap();
        assert_eq!(program_dirs(&home, "opencode", &path), [bin, real]);
    }

    /// Real processes under a real profile, so what the rules say is what
    /// happens. macOS only, like the fence.
    #[cfg(target_os = "macos")]
    mod enforced {
        use super::*;

        struct Machine {
            _root: tempfile::TempDir,
            home: PathBuf,
            session: PathBuf,
            profile: String,
        }

        fn machine() -> Machine {
            machine_with(false)
        }

        fn machine_with(home_readable: bool) -> Machine {
            // Seatbelt matches real paths, and the temp folder is behind a link.
            let root = tempfile::tempdir().unwrap();
            let base = root.path().canonicalize().unwrap();
            let home = base.join("home");
            let session = base.join("session");
            let scratch = base.join("scratch");
            for dir in [
                home.join(".npm"),
                home.join("Documents/vault"),
                home.join(".ssh"),
                session.clone(),
                scratch.clone(),
            ] {
                std::fs::create_dir_all(dir).unwrap();
            }
            std::fs::write(home.join("Documents/vault/note.md"), "the-secret-words").unwrap();
            std::fs::write(home.join(".ssh/id_rsa"), "PRIVATE KEY").unwrap();
            std::fs::write(home.join(".npm/cached.txt"), "package").unwrap();
            let profile = profile(&Fence {
                home: Some(home.clone()),
                state_dirs: vec![home.join(".npm")],
                scratch: Some(scratch),
                home_readable,
                ..Fence::default()
            });
            Machine {
                _root: root,
                home,
                session,
                profile,
            }
        }

        /// Runs `script` in `sh` under the profile: whether it worked, and
        /// what it printed.
        fn run(machine: &Machine, script: &str) -> (bool, String) {
            let out = std::process::Command::new("/usr/bin/sandbox-exec")
                .args(["-p", &machine.profile, "--", "/bin/sh", "-c", script])
                .current_dir(&machine.session)
                .output()
                .unwrap();
            let said = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            (out.status.success(), said)
        }

        #[test]
        fn the_agent_still_runs_and_reads_the_system() {
            let machine = machine();
            let (ok, said) = run(&machine, "cat /etc/hosts > /dev/null && echo fine");
            assert!(ok && said.contains("fine"), "{said}");
        }

        #[test]
        fn notes_and_keys_in_home_cannot_be_read() {
            let machine = machine();
            for secret in ["Documents/vault/note.md", ".ssh/id_rsa"] {
                let (ok, said) = run(
                    &machine,
                    &format!("cat '{}'", machine.home.join(secret).display()),
                );
                assert!(!ok, "{secret} was readable: {said}");
                assert!(!said.contains("the-secret-words") && !said.contains("PRIVATE KEY"));
            }
            let (ok, _) = run(
                &machine,
                &format!("ls '{}/Documents'", machine.home.display()),
            );
            assert!(!ok, "home's folders could be listed");
            let (ok, said) = run(
                &machine,
                &format!("cat '{}'", machine.home.join(".npm/cached.txt").display()),
            );
            assert!(ok && said.contains("package"), "{said}");
        }

        #[test]
        fn an_agent_that_resolves_home_can_name_home_but_not_open_it() {
            let machine = machine_with(true);
            // Home's own listing is readable: an agent that resolves the home
            // folder while it starts is satisfied by the names in it.
            let (ok, said) = run(&machine, &format!("ls '{}'", machine.home.display()));
            assert!(ok && said.contains("Documents"), "{said}");
            // Nothing inside home is listable or readable.
            for target in ["Documents", "Documents/vault", ".ssh"] {
                let (ok, said) = run(
                    &machine,
                    &format!("ls '{}'", machine.home.join(target).display()),
                );
                assert!(!ok, "{target} could be listed: {said}");
            }
            for secret in ["Documents/vault/note.md", ".ssh/id_rsa"] {
                let (ok, said) = run(
                    &machine,
                    &format!("cat '{}'", machine.home.join(secret).display()),
                );
                assert!(!ok, "{secret} was readable: {said}");
                assert!(!said.contains("the-secret-words") && !said.contains("PRIVATE KEY"));
            }
        }

        #[test]
        fn nothing_can_be_written_but_the_agents_own_state() {
            let machine = machine();
            let (ok, said) = run(
                &machine,
                &format!("echo x > '{}/.npm/new.txt'", machine.home.display()),
            );
            assert!(ok, "the npm cache must stay writable: {said}");

            for target in [
                machine.session.join("hello.txt"),
                machine.home.join("Documents/vault/new.md"),
                machine.home.join("Documents/new.txt"),
                PathBuf::from("/tmp/nuza-sandbox-escape"),
            ] {
                let (ok, _) = run(&machine, &format!("echo x > '{}'", target.display()));
                assert!(!ok && !target.exists(), "wrote to {}", target.display());
            }
            let note = machine.home.join("Documents/vault/note.md");
            let (ok, _) = run(&machine, &format!("rm -f '{}'", note.display()));
            assert!(!ok && note.exists(), "deleted a note");
        }

        #[test]
        fn what_the_agent_starts_is_fenced_in_too() {
            let machine = machine();
            let note = machine.home.join("Documents/vault/note.md");
            let (ok, said) = run(
                &machine,
                &format!("sh -c \"sh -c 'cat {}'\"", note.display()),
            );
            assert!(!ok && !said.contains("the-secret-words"), "{said}");
        }
    }
}
