//! The fence an agent runs inside on Linux: Landlock, applied to the agent's
//! process just before it starts, and so to everything it starts in turn.
//!
//! The policy is the Mac's (see `sandbox.rs`), in Landlock's terms. Landlock
//! only ever allows - nothing is readable unless a rule says so - so where the
//! Mac's profile denies the home folder and opens a few holes in it, this
//! opens the system's folders and those same few holes, and nothing else:
//!
//! - **Readable:** the system (`/usr`, `/etc`, `/proc`, ...), the temporary
//!   folder, the agent's own state, `~/.npmrc`, and the folders its programs
//!   live in. Not the home folder, so not the vault in it, nor `~/.ssh`; not
//!   `/mnt` or `/media`, so not a vault on another disk either.
//! - **Writable:** the agent's own state, its scratch folder, and the devices
//!   in `/dev` (a terminal, `/dev/null`). Nothing else.
//! - **The network** is not Landlock's business here: it stays open, as on
//!   the Mac, and fetching a web page as a tool is refused at the permission
//!   layer instead.
//!
//! Where the kernel has no Landlock (older than 5.13, or with it switched
//! off), nothing pretends otherwise: [`abi`] says there is none, `sandboxed()`
//! says false, and the consent dialog says the agent runs with the person's
//! own permissions.

use std::path::{Path, PathBuf};

use super::sandbox::Fence;

// Landlock's filesystem rights, from <linux/landlock.h>.
const EXECUTE: u64 = 1 << 0;
const WRITE_FILE: u64 = 1 << 1;
const READ_FILE: u64 = 1 << 2;
const READ_DIR: u64 = 1 << 3;
/// Every right of the first version: reading, writing, making and removing.
const ALL_V1: u64 = (1 << 13) - 1;
/// Moving a file from one folder to another (version 2).
const REFER: u64 = 1 << 13;
/// Truncating a file (version 3).
const TRUNCATE: u64 = 1 << 14;
/// The rights a rule on a single file may carry; anything more is refused.
const FILE_RIGHTS: u64 = EXECUTE | WRITE_FILE | READ_FILE | TRUNCATE;

/// What a rule lets the agent do beneath a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access {
    Read,
    ReadWrite,
    /// Read, and write to what is already there - for `/dev`.
    Device,
    /// A directory's own listing, and nothing inside it.
    List,
}

/// The system folders an agent may read, where they exist.
const SYSTEM: &[&str] = &[
    "/usr", "/bin", "/sbin", "/lib", "/lib32", "/lib64", "/libx32", "/etc", "/opt", "/proc",
    "/sys", "/run", "/var", "/snap", "/nix",
];

/// The folders the system lets an agent read: [`SYSTEM`], and `temp`.
pub(crate) fn system_reads(temp: &Path) -> Vec<PathBuf> {
    SYSTEM
        .iter()
        .map(PathBuf::from)
        .chain(std::iter::once(temp.to_path_buf()))
        .collect()
}

/// The rules for `fence`, given the folders the system is read from. A folder
/// that holds the home folder is never one of them - opening it would open
/// home - and neither is home itself.
pub(crate) fn rules(fence: &Fence, system: &[PathBuf]) -> Vec<(PathBuf, Access)> {
    let home = fence.home.as_deref();
    // Home, or any folder home is inside.
    let opens_home = |path: &Path| home.is_some_and(|home| home.starts_with(path));
    let mut rules = Vec::new();
    for dir in system {
        if opens_home(dir) {
            log::warn!(
                "not letting the agent read {}: the home folder is inside it",
                dir.display()
            );
            continue;
        }
        rules.push((dir.clone(), Access::Read));
    }
    rules.push((PathBuf::from("/dev"), Access::Device));
    for dir in &fence.read_dirs {
        if !opens_home(dir) {
            rules.push((dir.clone(), Access::Read));
        }
    }
    if let Some(home) = home {
        rules.push((home.join(".npmrc"), Access::Read));
        // The listing of home itself, where an agent resolves the home folder
        // as it starts. `READ_DIR` and no `READ_FILE`: what is in home can be
        // named, never read.
        if fence.home_readable {
            rules.push((home.to_path_buf(), Access::List));
        }
    }
    for dir in fence.state_dirs.iter().chain(&fence.scratch) {
        if !opens_home(dir) {
            rules.push((dir.clone(), Access::ReadWrite));
        }
    }
    // A file of the agent's own state, as it is: Landlock has no rule for a
    // file that does not exist yet.
    for file in &fence.state_files {
        if !opens_home(file) {
            rules.push((file.clone(), Access::ReadWrite));
        }
    }
    rules
}

/// The rights `access` comes to, under a kernel that knows `handled`.
fn rights(access: Access, handled: u64) -> u64 {
    let wanted = match access {
        Access::Read => EXECUTE | READ_FILE | READ_DIR,
        Access::List => EXECUTE | READ_DIR,
        Access::ReadWrite => handled,
        Access::Device => EXECUTE | READ_FILE | READ_DIR | WRITE_FILE | TRUNCATE,
    };
    wanted & handled
}

/// Every right a kernel speaking Landlock `abi` can hold back.
fn handled(abi: u32) -> u64 {
    let mut all = ALL_V1;
    if abi >= 2 {
        all |= REFER;
    }
    if abi >= 3 {
        all |= TRUNCATE;
    }
    all
}

#[cfg(target_os = "linux")]
pub(crate) use linux::{abi, confine, Ruleset};

#[cfg(target_os = "linux")]
mod linux {
    use std::ffi::CString;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
    use std::sync::Arc;

    use super::{handled, rights, Access, FILE_RIGHTS};

    const CREATE_RULESET_VERSION: libc::c_uint = 1;
    const RULE_PATH_BENEATH: libc::c_int = 1;

    #[repr(C)]
    struct RulesetAttr {
        handled_access_fs: u64,
    }

    #[repr(C, packed)]
    struct PathBeneathAttr {
        allowed_access: u64,
        parent_fd: i32,
    }

    /// The Landlock version this kernel speaks, if it has Landlock at all.
    pub(crate) fn abi() -> Option<u32> {
        // SAFETY: asking for the version passes no pointer the kernel reads.
        let version = unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                std::ptr::null::<RulesetAttr>(),
                0usize,
                CREATE_RULESET_VERSION,
            )
        };
        u32::try_from(version).ok().filter(|&version| version >= 1)
    }

    /// A finished set of rules, ready to be put on a process as it starts.
    pub(crate) struct Ruleset(Arc<OwnedFd>);

    impl Ruleset {
        /// Builds the rules into a ruleset. A path that is not there is left
        /// out; one that cannot be opened for any other reason fails it.
        pub(crate) fn build(rules: &[(std::path::PathBuf, Access)]) -> io::Result<Ruleset> {
            let abi = abi().ok_or_else(|| io::Error::other("this kernel has no Landlock"))?;
            let handled = handled(abi);
            let attr = RulesetAttr {
                handled_access_fs: handled,
            };
            // SAFETY: `attr` is a valid ruleset attribute of the size given.
            let fd = unsafe {
                libc::syscall(
                    libc::SYS_landlock_create_ruleset,
                    &attr as *const RulesetAttr,
                    std::mem::size_of::<RulesetAttr>(),
                    0 as libc::c_uint,
                )
            };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: the kernel just gave us this descriptor, and no one else.
            let ruleset = unsafe { OwnedFd::from_raw_fd(fd as i32) };
            for (path, access) in rules {
                add(&ruleset, path, rights(*access, handled))?;
            }
            Ok(Ruleset(Arc::new(ruleset)))
        }
    }

    fn add(ruleset: &OwnedFd, path: &Path, allowed: u64) -> io::Result<()> {
        let Ok(metadata) = std::fs::metadata(path) else {
            return Ok(());
        };
        let allowed = if metadata.is_dir() {
            allowed
        } else {
            allowed & FILE_RIGHTS
        };
        let name = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::other("a path with a NUL in it"))?;
        // SAFETY: `name` is a valid C string.
        let fd = unsafe { libc::open(name.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: just opened, and owned here.
        let parent = unsafe { OwnedFd::from_raw_fd(fd) };
        let attr = PathBeneathAttr {
            allowed_access: allowed,
            parent_fd: parent.as_raw_fd(),
        };
        // SAFETY: `attr` is a valid path-beneath rule; both descriptors are open.
        let added = unsafe {
            libc::syscall(
                libc::SYS_landlock_add_rule,
                ruleset.as_raw_fd(),
                RULE_PATH_BENEATH,
                &attr as *const PathBeneathAttr,
                0 as libc::c_uint,
            )
        };
        if added < 0 {
            let error = io::Error::last_os_error();
            return Err(io::Error::other(format!("{}: {error}", path.display())));
        }
        Ok(())
    }

    impl Ruleset {
        /// Puts the rules on this process, for good. Made between fork and
        /// exec, so it makes two system calls and nothing else - nothing is
        /// allocated.
        pub(crate) fn restrict_self(&self) -> io::Result<()> {
            // SAFETY: plain system calls on a descriptor that is open.
            unsafe {
                if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::syscall(libc::SYS_landlock_restrict_self, self.0.as_raw_fd(), 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        }
    }

    /// Puts `ruleset` on `command`'s process as it starts, before it runs
    /// anything: if it cannot be put on, the process does not start.
    pub(crate) fn confine(command: &mut tokio::process::Command, ruleset: Ruleset) {
        // SAFETY: what runs between fork and exec is `restrict_self`, which
        // is safe there.
        unsafe {
            command.pre_exec(move || ruleset.restrict_self());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fence() -> Fence {
        Fence {
            home: Some(PathBuf::from("/home/me")),
            state_dirs: vec![PathBuf::from("/home/me/.npm")],
            state_files: vec![PathBuf::from("/home/me/.claude.json")],
            read_dirs: vec![
                PathBuf::from("/home/me/.nvm/versions/node/v24"),
                PathBuf::from("/home/me"),
                PathBuf::from("/home"),
            ],
            scratch: Some(PathBuf::from("/tmp/nuza-agent-scratch")),
            home_readable: false,
        }
    }

    #[test]
    fn home_and_the_folders_it_is_in_are_never_opened() {
        let rules = rules(&fence(), &system_reads(Path::new("/tmp")));
        for (path, _) in &rules {
            assert!(
                !Path::new("/home/me").starts_with(path),
                "{} would open home",
                path.display()
            );
        }
        assert!(rules.contains(&(PathBuf::from("/home/me/.npm"), Access::ReadWrite)));
        assert!(rules.contains(&(
            PathBuf::from("/home/me/.nvm/versions/node/v24"),
            Access::Read
        )));
        assert!(rules.contains(&(PathBuf::from("/home/me/.npmrc"), Access::Read)));
    }

    #[test]
    fn an_agent_that_resolves_home_may_list_home_and_no_more() {
        let fence = Fence {
            home_readable: true,
            ..fence()
        };
        let rules = rules(&fence, &system_reads(Path::new("/tmp")));
        let (_, access) = rules
            .iter()
            .find(|(path, _)| path == Path::new("/home/me"))
            .expect("an agent that resolves home is given its listing");
        assert_eq!(*access, Access::List);
        // The listing only: opening a file in home is still refused.
        assert_eq!(rights(Access::List, handled(3)) & READ_FILE, 0);
    }

    #[test]
    fn a_system_folder_that_holds_home_is_left_out() {
        let fence = Fence {
            home: Some(PathBuf::from("/var/lib/someone")),
            ..Fence::default()
        };
        let rules = rules(&fence, &system_reads(Path::new("/tmp")));
        assert!(!rules.iter().any(|(path, _)| path == Path::new("/var")));
        assert!(rules.contains(&(PathBuf::from("/usr"), Access::Read)));
    }

    #[test]
    fn only_state_scratch_and_devices_are_written() {
        let writable: Vec<_> = rules(&fence(), &system_reads(Path::new("/tmp")))
            .into_iter()
            .filter(|(_, access)| *access != Access::Read)
            .map(|(path, _)| path)
            .collect();
        assert_eq!(
            writable,
            [
                PathBuf::from("/dev"),
                PathBuf::from("/home/me/.npm"),
                PathBuf::from("/tmp/nuza-agent-scratch"),
                PathBuf::from("/home/me/.claude.json"),
            ]
        );
    }

    #[test]
    fn rights_never_go_past_what_the_kernel_handles() {
        let v1 = handled(1);
        assert_eq!(v1 & (REFER | TRUNCATE), 0);
        assert_eq!(rights(Access::Device, v1) & TRUNCATE, 0);
        assert_eq!(
            rights(Access::Read, handled(3)),
            EXECUTE | READ_FILE | READ_DIR
        );
        assert_eq!(rights(Access::ReadWrite, handled(3)), handled(3));
    }

    /// The fence on a real kernel. Where it has no Landlock this says so and
    /// passes - unless `NUZA_REQUIRE_LANDLOCK` is set, as it is on CI, where a
    /// kernel without it fails the run rather than skipping the proof.
    #[cfg(target_os = "linux")]
    mod enforced {
        use super::*;
        use std::os::unix::process::CommandExt;

        struct Machine {
            _home_root: tempfile::TempDir,
            _temp_root: tempfile::TempDir,
            home: PathBuf,
            session: PathBuf,
            rules: Vec<(PathBuf, Access)>,
        }

        fn machine() -> Option<Machine> {
            if linux::abi().is_none() {
                assert!(
                    std::env::var("NUZA_REQUIRE_LANDLOCK").map_or(true, |set| set.is_empty()),
                    "this kernel has no Landlock, and the run requires it"
                );
                eprintln!("no Landlock on this kernel; the enforced tests are skipped");
                return None;
            }
            // Home outside the temporary folder, which the agent may read.
            let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
            std::fs::create_dir_all(&target).unwrap();
            let home_root = tempfile::tempdir_in(&target).unwrap();
            let home = home_root.path().join("home");
            let temp_root = tempfile::tempdir().unwrap();
            let session = temp_root.path().join("session");
            let scratch = temp_root.path().join("scratch");
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
            let fence = Fence {
                home: Some(home.clone()),
                state_dirs: vec![home.join(".npm")],
                scratch: Some(scratch),
                ..Fence::default()
            };
            let rules = rules(&fence, &system_reads(&std::env::temp_dir()));
            Some(Machine {
                _home_root: home_root,
                _temp_root: temp_root,
                home,
                session,
                rules,
            })
        }

        fn run(machine: &Machine, script: &str) -> (bool, String) {
            let ruleset = linux::Ruleset::build(&machine.rules).unwrap();
            let mut command = std::process::Command::new("/bin/sh");
            command.args(["-c", script]).current_dir(&machine.session);
            // SAFETY: as in `confine`.
            unsafe {
                command.pre_exec(move || ruleset.restrict_self());
            }
            let out = command.output().unwrap();
            let said = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            (out.status.success(), said)
        }

        #[test]
        fn the_agent_still_runs_and_reads_the_system() {
            let Some(machine) = machine() else { return };
            let (ok, said) = run(&machine, "cat /etc/hosts > /dev/null && echo fine");
            assert!(ok && said.contains("fine"), "{said}");
        }

        #[test]
        fn notes_and_keys_in_home_cannot_be_read() {
            let Some(machine) = machine() else { return };
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
        fn nothing_can_be_written_but_the_agents_own_state() {
            let Some(machine) = machine() else { return };
            let (ok, said) = run(
                &machine,
                &format!("echo x > '{}/.npm/new.txt'", machine.home.display()),
            );
            assert!(ok, "the npm cache must stay writable: {said}");

            for target in [
                machine.session.join("hello.txt"),
                machine.home.join("Documents/vault/new.md"),
                machine.home.join("Documents/new.txt"),
                std::env::temp_dir().join("nuza-sandbox-escape"),
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
            let Some(machine) = machine() else { return };
            let note = machine.home.join("Documents/vault/note.md");
            let (ok, said) = run(
                &machine,
                &format!("sh -c \"sh -c 'cat {}'\"", note.display()),
            );
            assert!(!ok && !said.contains("the-secret-words"), "{said}");
        }
    }
}
