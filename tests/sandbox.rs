//! What the confinement actually confines, asserted by running something
//! inside it and looking at what it could reach.
//!
//! **These do not test the argument list, they test the kernel's answer.**
//! A test that asserts `wrap` emitted `--ro-bind` proves the string was
//! built, never that anything was denied -- and the bug this file exists for
//! was invisible at that level: the arguments were exactly right, and a
//! read-only bind of the workdir still handed a confined adapter the user's
//! API key.
//!
//! Linux-gated on `bwrap` being present rather than `#[ignore]`d: on a host
//! without it `wrap` returns `None` by design, so there is no sandbox to make
//! claims about and skipping is the honest result. The macOS profile is
//! asserted as a string in `sandbox.rs`'s own tests, because Seatbelt cannot
//! be exercised from here.

#![cfg(target_os = "linux")]

use kobold::sandbox::{self, Policy};

/// Run `script` under the given policy and return its stdout.
fn confined(policy: Policy, script: &str) -> String {
    let cwd = std::env::current_dir().expect("a working directory");
    let sh = sandbox::resolve("sh").expect("sh is on PATH");
    let argv = vec![
        sh.to_string_lossy().into_owned(),
        "-c".to_owned(),
        script.to_owned(),
    ];
    let mut cmd = sandbox::wrap(policy, &cwd, &argv).expect("bwrap is present");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let out = rt.block_on(async move { cmd.output().await.expect("the confined child ran") });
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn have_bwrap() -> bool {
    sandbox::available().is_some()
}

/// The bug that made `share_workdir` exist, pinned from both sides.
///
/// **Asserting only that the key is unreachable would be satisfied by a
/// sandbox that blocks everything, including a broken one that never starts
/// the child.** So the same file is read under the sharing policy in the same
/// test: it must be reachable there. That pairing is the assertion -- one
/// half proves the denial, the other proves there was something to deny.
#[test]
fn an_adapter_cannot_read_the_credential_file_a_tool_can() {
    if !have_bwrap() {
        return;
    }
    let cwd = std::env::current_dir().expect("a working directory");
    // **In the workdir itself, deliberately not under `target/`.** An earlier
    // version put it there and it broke the moment build output moved off this
    // volume: `target/` became a symlink to `/cache`, which is outside the
    // bind, so the probe file was unreachable even under the sharing policy
    // and the test failed on its *positive* half. That is the half that exists
    // to stop the denial passing vacuously, so it did its job -- but the
    // lesson is that a fixture must not assume where build artefacts live.
    // `.env` is what this test is really about, and `.env` sits here.
    let secret = cwd.join(".sandbox-credential-probe");
    std::fs::write(&secret, "sk-not-a-real-key").expect("write the probe file");

    let script = format!(
        "[ -r '{}' ] && echo READABLE || echo blocked",
        secret.display()
    );

    let shared = confined(
        Policy {
            allow_network: false,
            writable_root: false,
            share_workdir: true,
        },
        &script,
    );
    assert!(
        shared.contains("READABLE"),
        "the probe file was unreachable even when the workdir is shared, so the \
         denial below would prove nothing: {shared:?}"
    );

    let adapter = confined(
        Policy {
            allow_network: false,
            writable_root: false,
            share_workdir: false,
        },
        &script,
    );
    assert!(
        adapter.contains("blocked"),
        "an adapter could read a file in the project tree; .env lives there: {adapter:?}"
    );

    let _ = std::fs::remove_file(&secret);
}

/// `allow_network` is the field the egress broker will hang off, so it has to
/// mean something today. Both spellings, for the same reason as above: a
/// sandbox where nothing can reach the network would pass the denial alone.
#[test]
fn the_network_policy_decides_whether_egress_works() {
    if !have_bwrap() {
        return;
    }
    // Loopback to a port nothing is on: connect() fails either way, but the
    // *reason* differs and only the unshared-namespace case reports the
    // network as unreachable. Using an address that must not be dialled from
    // a test -- a real host -- would make this a network-dependent test.
    let script = "(exec 3<>/dev/tcp/127.0.0.1/9 && echo connected || echo refused) 2>/dev/null; \
                  ip link show lo >/dev/null 2>&1 && echo has_lo || echo no_iface_tools; \
                  cat /proc/net/dev | grep -c ':' ";

    let open = confined(
        Policy {
            allow_network: true,
            writable_root: false,
            share_workdir: false,
        },
        script,
    );
    let closed = confined(
        Policy {
            allow_network: false,
            writable_root: false,
            share_workdir: false,
        },
        script,
    );

    // A fresh network namespace has loopback only; the host has loopback plus
    // at least one real interface. Counting interfaces is what distinguishes
    // them without depending on any host being reachable.
    let count = |s: &str| -> usize {
        s.lines()
            .last()
            .and_then(|l| l.trim().parse().ok())
            .unwrap_or(0)
    };
    assert!(
        count(&open) > count(&closed),
        "denying the network did not shrink the interface list, so --unshare-net \
         is not taking effect: open={open:?} closed={closed:?}"
    );
    assert!(
        count(&closed) <= 1,
        "a network-denied child still sees more than loopback: {closed:?}"
    );
}

/// The default policy is the one a careless caller gets, so it is asserted
/// directly rather than left to be inferred from the fields.
#[test]
fn the_default_policy_denies_the_network_and_all_writes() {
    if !have_bwrap() {
        return;
    }
    let out = confined(
        Policy::default(),
        "touch ./probe-write 2>/dev/null && echo wrote || echo readonly; \
         cat /proc/net/dev | grep -c ':'",
    );
    assert!(
        out.contains("readonly"),
        "the default policy allowed a write: {out:?}"
    );
    let ifaces: usize = out
        .lines()
        .last()
        .and_then(|l| l.trim().parse().ok())
        .unwrap_or(99);
    assert!(
        ifaces <= 1,
        "the default policy left the network reachable: {out:?}"
    );
}

/// Resolution happens in the parent, and the reason is that a wrapper hides
/// the failure otherwise: `bwrap` exists, so spawning it succeeds however
/// missing the adapter is.
#[test]
fn a_name_that_is_not_on_path_resolves_to_nothing() {
    assert!(sandbox::resolve("definitely-not-a-real-adapter-xyz").is_none());
    // The positive half: resolution must actually find things, or the check
    // above is satisfied by a function that always returns None.
    let sh = sandbox::resolve("sh").expect("sh is on PATH");
    assert!(
        sh.is_absolute(),
        "a resolved command must be absolute: {sh:?}"
    );
    assert!(sh.is_file(), "a resolved command must exist: {sh:?}");
}

/// A path with a separator is taken as a path, never searched for on `PATH`.
#[test]
fn a_path_is_used_as_given_and_not_searched_for() {
    assert!(sandbox::resolve("./definitely-not-here-xyz").is_none());
    // `sh` exists on PATH, so a *path* spelling that does not exist must
    // still fail -- otherwise the separator check is not being honoured.
    assert!(sandbox::resolve("./sh").is_none());
    let abs = sandbox::resolve("/bin/sh").expect("an absolute path resolves");
    assert!(abs.is_file());
}
