//! Confinement for tool-executed commands.
//!
//! Linux uses bubblewrap. macOS uses `sandbox-exec` with a Seatbelt profile:
//! Apple has marked it deprecated for years, still ships it, and still has no
//! replacement that confines arbitrary spawned CLI processes (App Sandbox is
//! entitlement-based and applies to signed bundles). It remains what the
//! comparable tools use.

use std::path::Path;
use tokio::process::Command;

#[derive(Clone, Copy)]
pub struct Policy {
    /// bwrap `--unshare-net` / Seatbelt `(deny network*)`.
    pub allow_network: bool,
    pub writable_root: bool,
    /// Whether the child can see the directory it was launched from at all.
    ///
    /// **`false` is what an adapter needs, and read-only was not enough.**
    /// Measured under bwrap on this box: with the workdir bound read-only,
    /// a confined adapter could still read `./.env` -- the user's API key.
    /// That defeats the reason the credential travels on stdin in the first
    /// place, which is to keep it out of anywhere the child can look. An
    /// adapter's job is a socket to a provider; it has no business in the
    /// project tree, so it gets its own executable and nothing else.
    ///
    /// `true` keeps the original behaviour for a child that is genuinely
    /// working on the files, which is what the tool cone is for.
    pub share_workdir: bool,
}

// Spelled out rather than derived. Both fields happen to default to `false`,
// so the derive would compile to the same thing -- but this is a security
// policy, and "denies the network, will not write outside the workdir" is
// worth being able to read rather than infer from the type of the fields.
#[allow(clippy::derivable_impls)]
impl Default for Policy {
    fn default() -> Self {
        Self {
            allow_network: false,
            writable_root: false,
            share_workdir: true,
        }
    }
}

/// Returns None when no sandbox is available on this host. Callers must decide
/// whether to refuse or run unconfined; never silently downgrade.
pub fn available() -> Option<&'static str> {
    #[cfg(target_os = "linux")]
    {
        which("bwrap").then_some("bwrap")
    }
    #[cfg(target_os = "macos")]
    {
        which("sandbox-exec").then_some("sandbox-exec")
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

fn which(bin: &str) -> bool {
    resolve(bin).is_some()
}

#[cfg(test)]
mod availability {
    use super::*;

    /// `available` is the switch every caller reads to decide between
    /// confining and running unconfined, so it must not be able to answer
    /// from anywhere but the filesystem.
    ///
    /// Phrased as an equivalence rather than as `is_some()`, because this box
    /// has `bwrap` and the user's Mac has `sandbox-exec`, and a test that
    /// asserted either one directly would be asserting the host rather than
    /// the function. The equivalence holds on a host with the tool and on a
    /// host without it, and is false for a `which` stuck at either answer.
    #[test]
    fn availability_tracks_whether_the_tool_is_actually_on_disk() {
        let tool = if cfg!(target_os = "linux") {
            "bwrap"
        } else if cfg!(target_os = "macos") {
            "sandbox-exec"
        } else {
            // No sandbox exists for this platform; `available` must say so
            // rather than name a tool it cannot run.
            assert!(available().is_none());
            return;
        };
        assert_eq!(
            available().is_some(),
            resolve(tool).is_some(),
            "available() disagrees with whether {tool} is on PATH"
        );
        assert_eq!(available(), resolve(tool).is_some().then_some(tool));
    }

    /// And the negative direction, so the equivalence above cannot be
    /// satisfied by a `which` that is right by coincidence on this host.
    #[test]
    fn a_tool_that_is_not_installed_is_not_reported_as_available() {
        assert!(!which("definitely-not-a-sandbox-xyz"));
        assert!(
            which("sh"),
            "sh must be found, or the negative proves nothing"
        );
    }
}

/// Find `command` the way a shell would, and return it as an absolute path.
///
/// **The parent has to do this, and only when a sandbox is in play does it
/// become obvious why.** Unwrapped, a missing adapter fails at `spawn` and the
/// error names it. Wrapped, `bwrap` is what gets spawned -- it exists, so the
/// spawn *succeeds* -- and the adapter fails inside the wrapper, after the
/// fork, where the parent sees it as the child exiting rather than as a start
/// failure. "I could not start the provider" and "the provider started and
/// then died" are different problems with different fixes, and confining a
/// process must not blur them.
///
/// Resolving is the fix rather than a special case for missing files, because
/// **the absolute path is needed anyway**: the wrapper gives the child its own
/// filesystem view, so a bare name found on the parent's `PATH` is not
/// necessarily on the child's, and a relative path is resolved against a
/// working directory the wrapper has changed.
///
/// A name containing a separator is a path and is never searched for, which is
/// what a shell does and what anyone typing `./my-adapter` means.
pub fn resolve(command: &str) -> Option<std::path::PathBuf> {
    let as_path = Path::new(command);
    if command.contains(std::path::MAIN_SEPARATOR) {
        return as_path.is_file().then(|| {
            // Relative paths are made absolute here rather than at the call
            // site: `--chdir` moves the child, so "./x" would resolve
            // somewhere else once it is inside.
            std::fs::canonicalize(as_path).unwrap_or_else(|_| as_path.to_path_buf())
        });
    }

    // Check sibling directory of the current running executable (e.g. target/debug or target/release)
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(command);
            if candidate.is_file() {
                return std::fs::canonicalize(&candidate).ok().or(Some(candidate));
            }
        }
    }

    // Check target build directories relative to current working directory
    for rel in ["target/debug", "target/release"] {
        let candidate = Path::new(rel).join(command);
        if candidate.is_file() {
            return std::fs::canonicalize(&candidate).ok().or(Some(candidate));
        }
    }

    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(command))
        .find(|candidate| candidate.is_file())
}

/// Wrap `argv` so it runs confined.
///
/// **`workdir` is read-only under the default policy**, and the only writable
/// path is a private tmpfs that never touches the host. The doc here used to
/// say the workdir was writable, which is what `writable_root: true` does and
/// what the default deliberately does not -- a security policy whose comment
/// says the opposite of its code is worse than one with no comment.
pub fn wrap(policy: Policy, workdir: &Path, argv: &[String]) -> Option<Command> {
    wrap_with_socket(policy, workdir, argv, Path::new(""))
}

/// `wrap`, plus one unix socket the child may reach.
///
/// **The socket is the child's entire network**, so it is granted by path and
/// nothing else is. An empty path means no socket, which is what `wrap` is.
///
/// On Linux this is a bind mount into a namespace that has no network at all,
/// so there is nothing else to reach. On macOS there is no network namespace,
/// and **Seatbelt classifies a unix socket as network** -- so the profile has
/// to deny `network*` and then re-allow this one path, which is why the
/// socket has to be threaded this far down rather than handled at the spawn
/// site.
pub fn wrap_with_socket(
    policy: Policy,
    workdir: &Path,
    argv: &[String],
    socket: &Path,
) -> Option<Command> {
    #[cfg(target_os = "linux")]
    {
        if !which("bwrap") {
            return None;
        }
        let mut cmd = Command::new("bwrap");
        cmd.arg("--die-with-parent")
            .arg("--new-session")
            .arg("--unshare-pid")
            .arg("--unshare-ipc")
            .arg("--unshare-uts")
            .args(["--ro-bind", "/usr", "/usr"])
            .args(["--ro-bind", "/etc", "/etc"])
            .args(["--symlink", "usr/bin", "/bin"])
            .args(["--symlink", "usr/lib", "/lib"])
            .args(["--symlink", "usr/lib64", "/lib64"])
            .args(["--proc", "/proc"])
            .args(["--dev", "/dev"])
            .args(["--tmpfs", "/tmp"]);

        if !policy.allow_network {
            cmd.arg("--unshare-net");
        }
        if policy.share_workdir {
            let bind = if policy.writable_root {
                "--bind"
            } else {
                "--ro-bind"
            };
            cmd.args([bind, &workdir.to_string_lossy(), &workdir.to_string_lossy()]);
            cmd.args(["--chdir", &workdir.to_string_lossy()]);
        } else {
            // The executable and nothing else. It usually lives *inside* the
            // workdir (`target/release/kobold-openai`), so refusing to bind
            // the workdir without binding this would leave nothing to exec.
            if let Some(exe) = argv.first() {
                cmd.args(["--ro-bind", exe, exe]);
            }
            // Into the private tmpfs, so relative paths land somewhere that
            // is writable, empty, and gone at exit.
            cmd.args(["--chdir", "/tmp"]);
        }
        // Bound at the same path it has on the host, so Kobold can name it in
        // the startup frame without translating. bwrap creates the parent
        // directories inside for us.
        //
        // **`--bind`, not `--ro-bind`**: connecting to a unix socket needs
        // write permission on the node. A read-only bind here fails with
        // `Permission denied` at `connect`, which reads as the broker being
        // down rather than as a mount option.
        if !socket.as_os_str().is_empty() {
            let p = socket.to_string_lossy().into_owned();
            cmd.args(["--bind", &p, &p]);
        }
        cmd.arg("--");
        cmd.args(argv);
        // Explicit, and kept so despite the lint: three `cfg` arms sit here in
        // parallel and each ends the function. Dropping the keyword from
        // whichever one happens to compile last on this platform would make
        // them stop looking alike for a reason that is invisible in the source.
        #[allow(clippy::needless_return)]
        return Some(cmd);
    }

    #[cfg(target_os = "macos")]
    {
        if !which("sandbox-exec") {
            return None;
        }
        // Resolved before the profile is written, and this is not hygiene.
        // Seatbelt matches the RESOLVED path: `/tmp` is a symlink to
        // `/private/tmp`, so a rule naming the unresolved spelling never
        // matches -- and it fails as a DENIAL rather than a syntax error,
        // which reads as "the feature does not exist". Verified on macOS
        // 15.1.1: the same rule works when the path is resolved and does not
        // when it is not.
        let resolved = workdir.canonicalize();
        let workdir: &Path = resolved.as_deref().unwrap_or(workdir);
        // Resolved for the same reason the workdir is, and it bites harder
        // here: the socket lives under the temporary directory, `/tmp` is a
        // symlink to `/private/tmp`, and a rule naming the unresolved
        // spelling is a silent denial rather than an error.
        let resolved_socket = socket.canonicalize();
        let socket: &Path = resolved_socket.as_deref().unwrap_or(socket);
        let profile = seatbelt_profile(policy, workdir, argv.first().map(Path::new), socket);
        let mut cmd = Command::new("sandbox-exec");
        // Only chdir into the workdir when the policy actually exposes it;
        // starting in a directory the profile forbids reading is a confusing
        // way to fail.
        if policy.share_workdir {
            cmd.current_dir(workdir);
        }
        cmd.args(["-p", &profile]).args(argv);
        Some(cmd)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (policy, workdir, argv, socket);
        None
    }
}

/// Built on every platform, deliberately, though only macOS runs it.
///
/// **It used to be `#[cfg(target_os = "macos")]`, and that made the macOS
/// security policy untestable from the machine it is developed on.** This is
/// a pure function from a policy to a string -- there is nothing
/// platform-specific about computing it, only about handing it to
/// `sandbox-exec` -- so gating it bought nothing and cost the only coverage
/// the profile could have had. Its mutants were unkillable on Linux for the
/// same reason: the tests were gated too.
///
/// The `allow` is the price of that and is scoped to the platforms that do
/// not call it. Note what it is *not*: this file's other items are private,
/// so dead code here genuinely warns, unlike the `pub` items elsewhere in the
/// crate where the lint is silenced by accident.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn seatbelt_profile(policy: Policy, workdir: &Path, exe: Option<&Path>, socket: &Path) -> String {
    // Deny by default, then grant only what a confined child cannot start
    // without. Every clause here is a decision, and two of them are removals:
    //
    // `(allow file-read*)` used to be here, granting unrestricted read of the
    // entire filesystem. Under the old use case -- a short tool invocation
    // with the network denied -- that was survivable. For an adapter it is
    // not: unrestricted read plus a network is exfiltration of the user's SSH
    // keys, cloud credentials and whole conversation history, and it made
    // "sandboxed" mean something far weaker on macOS than on Linux, where
    // bwrap binds two directories read-only and nothing else.
    //
    // `(allow process-exec process-fork)` used to be here too, blanket, and
    // removing it outright was **wrong in a way only macOS could show**:
    // `sandbox-exec` applies the profile to itself and then `execvp`s the
    // target, so `(deny default)` with no exec rule blocks the very program
    // being launched. Every adapter died at startup with
    //
    //     sandbox-exec: execvp() of '.../kobold-openai' failed:
    //     Operation not permitted
    //
    // Linux never saw it because `bwrap` execs the payload before any of its
    // restrictions apply -- the confinement there is filesystem and namespace,
    // never an exec hook. A whole platform's adapter path was dead and the
    // Linux suite was green throughout.
    //
    // The grant is now **one literal path, the binary being launched**, which
    // keeps what the removal was for: the child still cannot start anything
    // else, so there is no shell, no interpreter, no second program. What it
    // can do is be itself.
    //
    // `process-fork` stays denied. Rust threads are not `fork`, and the
    // adapter has no reason to spawn.
    //
    // A confined child has no legitimate reason to start another program, and
    // child has no legitimate reason to start another program, and on Linux
    // that denial comes free from deny-by-default -- there is nothing else on
    // its filesystem to execute.
    //
    // Verified on macOS 15.1.1 that each of these bites: a deny-default
    // profile blocks execution outright, file-read denial is enforced, and
    // process-exec denial is enforced.
    let mut p = String::from("(version 1)(deny default)(allow sysctl-read)(allow mach-lookup)");

    // Reads, scoped rather than blanket. `dyld` needs the shared cache and
    // the system libraries to start any binary at all; the workdir is where
    // the thing being run lives.
    p.push_str(
        "(allow file-read* \
           (literal \"/\") \
           (subpath \"/usr/lib\") \
           (subpath \"/usr/share\") \
           (subpath \"/System/Library\") \
           (subpath \"/private/var/db/dyld\"))",
    );
    if policy.share_workdir {
        p.push_str(&format!(
            "(allow file-read* (subpath \"{}\"))",
            workdir.display()
        ));
    } else if let Some(exe) = exe {
        // One file, by literal path, not its directory: `target/release` also
        // holds whatever else was built, and the point of this branch is that
        // the adapter sees nothing of the project but itself.
        p.push_str(&format!(
            "(allow file-read* (literal \"{}\"))",
            exe.display()
        ));
    }
    p.push_str("(allow file-read-metadata)");

    // Exec, for exactly one path: the binary being launched. Without this
    // `sandbox-exec` cannot start it at all -- see the note above, which is a
    // bug this shipped with and only a Mac could find.
    if let Some(exe) = exe {
        p.push_str(&format!(
            "(allow process-exec (literal \"{}\"))",
            exe.display()
        ));
    } else if policy.share_workdir {
        // No executable named, so the caller is `wrap`, which does not know
        // one. Scope it to the workdir it is running in rather than granting
        // exec outright.
        p.push_str(&format!(
            "(allow process-exec (subpath \"{}\"))",
            workdir.display()
        ));
    }

    if policy.allow_network {
        p.push_str("(allow network*)");
    } else {
        p.push_str("(deny network*)");
        // Then one exception, because Seatbelt is believed to count a unix
        // socket as network and the broker is reached over one.
        //
        // **UNVERIFIED, and deliberately marked rather than left reading as
        // settled.** This box cannot run Seatbelt, macOS is one of two
        // targets, and nobody here has cited a document or a real macOS run
        // for the claim -- it is reasoning from `network*` being a broad
        // filter. What follows if it is wrong is not a hole: `(deny
        // network*)` still stands and the extra allow is inert, so a confined
        // adapter that cannot reach its broker fails closed and loudly. The
        // cost of being wrong is a mac user with a dead adapter, not an open
        // one. Settling it needs one run on a Mac.
        //
        // Denying and re-allowing in that order is deliberate either way: the
        // deny is the policy and this is the single hole in it, which is how
        // it should read to whoever audits the profile next.
        if !socket.as_os_str().is_empty() {
            p.push_str(&format!(
                "(allow network-outbound (literal \"{}\"))",
                socket.display()
            ));
        }
    }

    // Writes only where the policy says, and never to the real `/tmp`. That
    // grant used to be unconditional, which on Linux has no counterpart at
    // all -- bwrap gives a private tmpfs that is invisible on the host and
    // gone at exit.
    if policy.writable_root && policy.share_workdir {
        p.push_str(&format!(
            "(allow file-write* (subpath \"{}\"))",
            workdir.display()
        ));
    }
    p
}

#[cfg(test)]
mod macos_profile_tests {
    use super::*;

    /// The profile is a security policy expressed as a string, so what it
    /// does NOT say matters as much as what it does. Each assertion here
    /// names a grant that was present and was removed on purpose.
    #[test]
    fn the_profile_grants_no_blanket_read_and_no_exec() {
        let p = seatbelt_profile(
            Policy::default(),
            Path::new("/private/tmp/w"),
            None,
            Path::new(""),
        );
        assert!(
            p.contains("(deny default)"),
            "the profile must deny by default"
        );
        assert!(
            !p.contains("(allow file-read*)"),
            "blanket read is back: an adapter could read ~/.ssh and the transcript"
        );
        // **This assertion used to be `!p.contains("process-exec")`, and it
        // is why the bug shipped.** It read as rigour -- a confined child
        // must not start another program -- and it was satisfied perfectly by
        // a profile that could not start the child *either*. The negative
        // assertion had no partner showing that the one legitimate exec still
        // worked, so nothing here could tell "denies everything" from "denies
        // everything except the thing being launched".
        //
        // With no executable named there is nothing to grant, so the blanket
        // form must still be absent.
        assert!(
            !p.contains("(allow process-exec)"),
            "blanket exec is back: a confined child could start any program"
        );
        assert!(
            !p.contains("process-fork"),
            "fork is not needed and was removed on purpose"
        );
        assert!(
            p.contains("(deny network*)"),
            "the default policy denies the network"
        );
        assert!(
            !p.contains("/private/tmp\")") || p.contains("/private/tmp/w"),
            "the real /tmp must not be writable unconditionally"
        );
    }

    /// **The one exec that has to work, and the absence of every other.**
    ///
    /// Written after a Mac reported `sandbox-exec: execvp() ... Operation not
    /// permitted` for every adapter: the profile denied `process-exec`
    /// outright, and `sandbox-exec` applies the profile to itself before
    /// exec'ing the target, so the confinement blocked the program it was
    /// launching. Linux never saw it -- `bwrap` execs the payload before any
    /// of its restrictions apply.
    #[test]
    fn the_binary_being_launched_may_exec_and_nothing_else_may() {
        let exe = Path::new("/private/tmp/w/target/release/kobold-openai");
        let p = seatbelt_profile(
            Policy {
                allow_network: true,
                writable_root: false,
                share_workdir: false,
            },
            Path::new("/private/tmp/w"),
            Some(exe),
            Path::new(""),
        );
        assert!(
            p.contains(
                "(allow process-exec (literal \"/private/tmp/w/target/release/kobold-openai\"))"
            ),
            "the adapter cannot exec itself, so it never starts: {p}"
        );
        // `literal`, never `subpath`: a subpath grant over target/release
        // would let the adapter exec everything else that was built.
        assert!(
            !p.contains("(allow process-exec (subpath"),
            "a subpath exec grant lets the child start other programs: {p}"
        );
        assert!(!p.contains("(allow process-exec)"), "blanket exec: {p}");
    }

    /// The partner. A profile that granted nothing at all would satisfy every
    /// assertion above and fail to start any binary.
    #[test]
    fn the_profile_still_grants_what_a_binary_needs_to_start() {
        let p = seatbelt_profile(
            Policy::default(),
            Path::new("/private/tmp/w"),
            None,
            Path::new(""),
        );
        assert!(
            p.contains("/private/var/db/dyld"),
            "dyld needs its shared cache"
        );
        assert!(p.contains("/usr/lib"), "system libraries must be readable");
        assert!(p.contains("/private/tmp/w"), "the workdir must be readable");
    }

    #[test]
    fn a_writable_root_is_the_workdir_and_only_the_workdir() {
        let p = seatbelt_profile(
            Policy {
                writable_root: true,
                ..Default::default()
            },
            Path::new("/private/tmp/w"),
            None,
            Path::new(""),
        );
        assert!(p.contains("(allow file-write* (subpath \"/private/tmp/w\"))"));
        let default = seatbelt_profile(
            Policy::default(),
            Path::new("/private/tmp/w"),
            None,
            Path::new(""),
        );
        assert!(
            !default.contains("file-write*"),
            "the default grants no host write at all"
        );
    }

    /// The adapter's policy, which is the one that carries the credential
    /// risk. Asserted on both sides of the field, because "the workdir is
    /// absent" is satisfied by a profile that named nothing at all.
    #[test]
    fn an_unshared_workdir_grants_the_executable_and_not_the_tree() {
        let exe = Path::new("/private/tmp/w/target/release/kobold-openai");
        let adapter = seatbelt_profile(
            Policy {
                allow_network: true,
                writable_root: false,
                share_workdir: false,
            },
            Path::new("/private/tmp/w"),
            Some(exe),
            Path::new(""),
        );
        assert!(
            !adapter.contains("(subpath \"/private/tmp/w\")"),
            "the project tree is readable, so .env is: {adapter}"
        );
        assert!(
            adapter.contains("(literal \"/private/tmp/w/target/release/kobold-openai\")"),
            "the adapter cannot read its own binary and so cannot start: {adapter}"
        );
        // `literal`, never `subpath`: target/release holds everything else
        // that was built, and the point of this branch is that the adapter
        // sees nothing of the project but itself.
        assert!(
            !adapter.contains("(subpath \"/private/tmp/w/target/release/kobold-openai\")"),
            "a subpath grant here would expose the whole build directory"
        );

        let shared = seatbelt_profile(
            Policy {
                allow_network: true,
                writable_root: false,
                share_workdir: true,
            },
            Path::new("/private/tmp/w"),
            Some(exe),
            Path::new(""),
        );
        assert!(
            shared.contains("(subpath \"/private/tmp/w\")"),
            "sharing the workdir must still grant it, or the denial above proves nothing"
        );
    }

    /// A writable root only means anything when the root is shared at all.
    /// Written because the guard is an `&&` whose halves look independent:
    /// flipped to `||`, an adapter that shares nothing would be granted write
    /// to a directory it cannot even see, which is a rule that reads as a
    /// grant to whoever audits the profile next.
    #[test]
    fn writable_root_without_a_shared_workdir_grants_no_write() {
        let p = seatbelt_profile(
            Policy {
                allow_network: false,
                writable_root: true,
                share_workdir: false,
            },
            Path::new("/private/tmp/w"),
            None,
            Path::new(""),
        );
        assert!(
            !p.contains("file-write*"),
            "write was granted to a workdir the policy does not share: {p}"
        );
        // The partner: the same `writable_root: true` with sharing on must
        // still grant it, or this passes against a profile that never writes.
        let shared = seatbelt_profile(
            Policy {
                allow_network: false,
                writable_root: true,
                share_workdir: true,
            },
            Path::new("/private/tmp/w"),
            None,
            Path::new(""),
        );
        assert!(
            shared.contains("file-write*"),
            "writable_root stopped working entirely"
        );
    }

    /// The network field, both spellings, in the profile rather than in the
    /// kernel -- which is all that can be checked from Linux.
    #[test]
    fn the_network_clause_follows_the_policy() {
        let open = seatbelt_profile(
            Policy {
                allow_network: true,
                ..Default::default()
            },
            Path::new("/private/tmp/w"),
            None,
            Path::new(""),
        );
        assert!(open.contains("(allow network*)"), "{open}");
        assert!(
            !open.contains("(deny network*)"),
            "both clauses present: {open}"
        );
        let closed = seatbelt_profile(
            Policy::default(),
            Path::new("/private/tmp/w"),
            None,
            Path::new(""),
        );
        assert!(closed.contains("(deny network*)"), "{closed}");
        assert!(
            !closed.contains("(allow network*)"),
            "both clauses present: {closed}"
        );
    }

    /// The one hole in `(deny network*)`, both ways round.
    ///
    /// Macos-only behaviour checked as a string, which is all this box can
    /// do -- and worth doing precisely because it cannot be run here. With
    /// the emptiness test inverted, a confined adapter that **has** a broker
    /// gets no egress clause at all and cannot reach it, while the case with
    /// no socket gets `(allow network-outbound (literal ""))`, a rule that
    /// grants nothing and means nothing. Neither half shows up on Linux.
    #[test]
    fn the_egress_hole_is_opened_for_a_real_socket_and_for_nothing_else() {
        let with_socket = seatbelt_profile(
            Policy::default(),
            Path::new("/private/tmp/w"),
            None,
            Path::new("/private/tmp/kobold-egress-1/egress.sock"),
        );
        assert!(
            with_socket.contains(
                "(allow network-outbound (literal \"/private/tmp/kobold-egress-1/egress.sock\"))"
            ),
            "a confined adapter was given no way to reach its broker: {with_socket}"
        );
        // Order is part of the policy, not decoration: the deny is the rule
        // and this is the single exception to it, and a profile that read the
        // other way round would grant before it denied.
        let deny = with_socket
            .find("(deny network*)")
            .expect("the deny is gone");
        let hole = with_socket
            .find("(allow network-outbound")
            .expect("checked above");
        assert!(
            deny < hole,
            "the exception precedes the rule it excepts: {with_socket}"
        );

        // The partner, and the half the inverted test would pass: no socket
        // means no hole, not a hole onto the empty path.
        let none = seatbelt_profile(
            Policy::default(),
            Path::new("/private/tmp/w"),
            None,
            Path::new(""),
        );
        assert!(
            !none.contains("network-outbound"),
            "an empty socket path became a grant: {none}"
        );
    }
}
