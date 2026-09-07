//! Kobold's side of the adapter boundary, driven by a scripted fake.
//!
//! Hermetic: `fake-adapter` speaks the protocol and knows nothing about any
//! provider, so none of this touches a network. What is exercised is the part
//! that used to be a function call and is now a process -- the handshake, the
//! command path, and what Kobold makes of an adapter that stops talking.

use kobold::adapter::Adapter;
use kobold::net::{Command, Incoming, IncomingFrame, Model, Startup, Transport};

/// Serialises the tests that spawn an adapter.
///
/// `adapter::LIVE` is one process-global pid, because a Kobold process has
/// one adapter and the panic hook cannot borrow. That makes these tests
/// interfere: one test's spawn overwrites the pid another is about to kill,
/// and `kill_live` from one kills another's child.
///
/// Tokio's mutex rather than the standard one because the guard is held
/// across awaits for the whole of each test -- which is the point, since the
/// awaits are where the interference happens. It also has no poisoning, so
/// one failing test cannot fail the rest.
static SPAWNING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn spawning() -> tokio::sync::MutexGuard<'static, ()> {
    SPAWNING.lock().await
}

fn fake() -> String {
    let mut p = std::env::current_exe().expect("test exe path");
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    let candidate = p.join("fake-adapter");
    if !candidate.exists() {
        let _ = std::process::Command::new("cargo")
            .args(["build", "--bin", "fake-adapter"])
            .output();
    }
    candidate.to_string_lossy().into_owned()
}

fn startup() -> Startup {
    Startup {
        api_key: "sk-not-a-real-key".to_owned(),
        egress: None,
        model: Model {
            name: "test-model".to_owned(),
            effort: "none".to_owned(),
            server_tools: Vec::new(),
            tools: vec![("t".to_owned(), "a tool".to_owned(), "{}".to_owned())],
        },
    }
}

/// A frame reduced to `(lane, what happened)`. Comparing whole frames would
/// mean spelling out an empty `Base` on every row for no gain.
fn summary(f: &IncomingFrame) -> (String, String) {
    match f {
        IncomingFrame::Transport(Transport::Connected) => (String::new(), "connected".to_owned()),
        IncomingFrame::Transport(Transport::Disconnected(why)) => {
            (String::new(), format!("disconnected:{why}"))
        }
        IncomingFrame::Event { lane, event } => (
            lane.clone(),
            match event {
                Incoming::TextMessageContent { delta, .. } => format!("delta:{delta}"),
                Incoming::RunFinished { .. } => "finished".to_owned(),
                Incoming::RunError { code, .. } => {
                    format!("error:{}", code.clone().unwrap_or_default())
                }
                other => format!("unexpected:{other:?}"),
            },
        ),
    }
}

fn summaries(frames: &[IncomingFrame]) -> Vec<(String, String)> {
    frames.iter().map(summary).collect()
}

/// Spawns the fake with a script, sends one turn, and drains everything
/// Kobold received until the adapter's stdout closes.
async fn spawn_with(script: &str, lane: &str) -> Vec<IncomingFrame> {
    let (mut adapter, cmd_tx, mut rx) =
        Adapter::spawn(&fake(), &[script.to_owned()], &startup(), &[])
            .await
            .expect("spawn the fake");
    let _ = cmd_tx.send(Command::Send {
        lane: lane.to_owned(),
        text: "hello".to_owned(),
        previous_response_id: None,
        replay: Vec::new(),
    });
    let mut got = Vec::new();
    while let Some(u) = rx.recv().await {
        got.push(u);
    }
    adapter.shutdown().await;
    got
}

#[tokio::test]
async fn a_scripted_turn_arrives_as_the_updates_it_scripted() {
    let _spawning = spawning().await;
    // The happy path, and the proof the handshake landed: the fake refuses to
    // start without a well-formed startup frame, so reaching `Connected` at
    // all means the frame parsed on the far side of a real pipe.
    let got = spawn_with("await,connected,delta:hello ,delta:world,complete", "main").await;
    assert_eq!(
        summaries(&got),
        vec![
            (String::new(), "connected".to_owned()),
            ("main".to_owned(), "delta:hello ".to_owned()),
            ("main".to_owned(), "delta:world".to_owned()),
            ("main".to_owned(), "finished".to_owned()),
            // Stdout closing at the end of the script is the adapter exiting,
            // which Kobold must always report rather than leaving a turn to
            // hang. See the test below for why that matters.
            (String::new(), "disconnected:the adapter exited".to_owned()),
        ]
    );
}

/// **The adapter is told where its broker is, and nothing checked that.**
///
/// Deleting the `egress` field from the `Startup` expression in
/// `Adapter::spawn` survived mutation: the field defaults to `None`, so a
/// confined adapter would be handed no socket path at all and would have no
/// way to reach the network it is otherwise denied. The failure is total and
/// silent -- it looks like a provider outage.
///
/// Observed through the fake reporting back what it was given, rather than by
/// reading the `Startup` we passed in: what matters is what crossed the pipe
/// and was parsed on the other side, not what we wrote before it was
/// serialised.
#[tokio::test]
async fn the_adapter_is_told_where_its_egress_socket_is() {
    let _spawning = spawning().await;
    let told: Vec<String> = spawn_with("await,egress", "main")
        .await
        .into_iter()
        .filter_map(|f| match f {
            IncomingFrame::Event {
                event: Incoming::TextMessageContent { delta, .. },
                ..
            } => Some(delta),
            _ => None,
        })
        .collect();
    let path = told
        .first()
        .map(String::as_str)
        .unwrap_or("<nothing at all>");
    assert_ne!(
        path, "<none>",
        "the adapter was told nothing about its egress socket"
    );
    // Named for what it is, and inside a per-adapter directory. Asserting it
    // is merely non-empty would pass for any string that happened to be set.
    assert!(
        path.ends_with("/egress.sock"),
        "not a broker socket path: {path:?}"
    );
    assert!(
        path.contains("kobold-egress-"),
        "not in a per-adapter hutch: {path:?}"
    );
    // Deliberately *not* asserting the socket exists now: by the time these
    // frames are read the adapter has shut down and its hutch has dropped, so
    // the path is gone -- which is the teardown `tests/broker.rs` asserts
    // directly. Checking it here would be checking the wrong moment.
}

#[tokio::test]
async fn the_command_reaches_the_adapter_and_names_its_lane() {
    let _spawning = spawning().await;
    // The counter-assertion to everything above: every one of those updates
    // is scripted, so they would arrive whatever Kobold sent -- or if it sent
    // nothing at all. The fake sets its lane from the command it receives, so
    // a lane of "fork-1" coming back is the command having crossed the pipe.
    let lanes: Vec<String> = spawn_with("await,delta:x", "fork-1")
        .await
        .into_iter()
        .filter_map(|u| match u {
            IncomingFrame::Event {
                lane,
                event: Incoming::TextMessageContent { .. },
            } => Some(lane),
            _ => None,
        })
        .collect();
    assert_eq!(
        lanes,
        vec!["fork-1".to_owned()],
        "the command did not reach the adapter"
    );
}

#[tokio::test]
async fn an_adapter_that_dies_mid_turn_is_reported_rather_than_hanging() {
    let _spawning = spawning().await;
    // The failure this whole boundary adds. A dead socket used to end a
    // session silently and that was worth a commit to fix; a dead adapter is
    // the same defect one layer out, and there are more ways to die.
    let got = spawn_with("await,connected,delta:half a rep,exit:1", "main").await;
    assert!(
        matches!(
            got.last(),
            Some(IncomingFrame::Transport(Transport::Disconnected(_)))
        ),
        "a dead adapter must end in Disconnected, got {got:?}"
    );
    // And the partial output before it is kept: what the model already said
    // is still what it said.
    assert!(
        got.iter().any(|u| summary(u).1 == "delta:half a rep"),
        "the delta before the death was dropped"
    );
}

#[tokio::test]
async fn a_non_protocol_line_is_reported_and_names_what_arrived() {
    let _spawning = spawning().await;
    // The realistic cause is an adapter printing to stdout instead of stderr.
    // Skipping it would leave the turn hanging with no reason given.
    let got = spawn_with("await,connected,garbage,complete", "main").await;
    let Some(IncomingFrame::Transport(Transport::Disconnected(why))) = got
        .iter()
        .find(|f| matches!(f, IncomingFrame::Transport(Transport::Disconnected(_))))
    else {
        panic!("garbage on stdout was not reported: {got:?}");
    };
    assert!(
        why.contains("reconnecting"),
        "the reason must quote the line, got {why:?}"
    );
}

#[tokio::test]
async fn a_binary_that_does_not_exist_fails_to_spawn_and_says_which() {
    let _spawning = spawning().await;
    // Distinct from an authentication failure, which happens later and inside
    // the adapter. "I could not start the provider" and "the provider refused
    // your key" are different problems with different fixes.
    let Err(err) = Adapter::spawn("definitely-not-a-real-adapter-xyz", &[], &startup(), &[]).await
    else {
        panic!("a missing binary should not have started");
    };
    let shown = err.to_string();
    assert!(
        shown.contains("definitely-not-a-real-adapter-xyz"),
        "unhelpful: {shown}"
    );
}

#[tokio::test]
async fn a_blank_line_from_the_adapter_is_ignored_rather_than_decoded() {
    let _spawning = spawning().await;
    // A pipe can carry an empty line -- a stray flush, a wrapper script's
    // newline. Decoding one would report the adapter as broken and end the
    // turn, so it has to pass through without ending anything.
    let got = spawn_with("await,connected,blank,delta:still here,complete", "main").await;
    assert!(
        got.iter().any(|u| summary(u).1 == "delta:still here"),
        "a blank line stopped the stream: {got:?}"
    );
    assert!(
        got.iter().any(|u| summary(u).1 == "finished"),
        "the turn should have completed: {got:?}"
    );
    // And it was not mistaken for a broken adapter before the turn ended.
    let first_disconnect = got
        .iter()
        .position(|f| matches!(f, IncomingFrame::Transport(Transport::Disconnected(_))));
    let completed = got.iter().position(|u| summary(u).1 == "finished");
    assert!(
        completed < first_disconnect,
        "reported a failure before completing: {got:?}"
    );
}

#[tokio::test]
async fn shutdown_actually_kills_an_adapter_that_will_not_leave() {
    let _spawning = spawning().await;
    // The one that matters on macOS, which has no `--die-with-parent`: a
    // wedged adapter holds the credential it was given and keeps a socket
    // open, so Kobold has to kill it rather than hope.
    //
    // `silent` reads its commands and then blocks forever. If `shutdown` did
    // nothing the child would still be running and its stdout still open, so
    // the receiver would never close -- which is what the timeout catches.
    let (mut adapter, cmd_tx, mut rx) =
        Adapter::spawn(&fake(), &["silent".to_owned()], &startup(), &[])
            .await
            .expect("spawn");
    assert!(adapter.id().is_some(), "the adapter should be running");
    let _ = cmd_tx.send(Command::Send {
        lane: "main".to_owned(),
        text: "hello".to_owned(),
        previous_response_id: None,
        replay: Vec::new(),
    });

    adapter.shutdown().await;
    assert!(adapter.id().is_none(), "the child was not reaped");

    // Its stdout is closed now, so the update stream ends. A generous
    // timeout: this asserts termination, not speed.
    let closed = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while rx.recv().await.is_some() {}
    })
    .await;
    assert!(closed.is_ok(), "the adapter outlived shutdown");
}

#[tokio::test]
async fn backpressure_holds_a_flood_instead_of_queueing_it_without_limit() {
    let _spawning = spawning().await;
    // An adapter can produce updates far faster than a terminal shows them.
    // Unbounded, that is unbounded memory in Kobold; bounded, the adapter's
    // own write blocks and it slows to the rate Kobold consumes.
    //
    // What is asserted is that every update still arrives -- backpressure
    // must slow the producer, never drop from the middle of a reply.
    let (mut adapter, cmd_tx, mut rx) = Adapter::spawn(
        &fake(),
        &["await,flood:2000,complete".to_owned()],
        &startup(),
        &[],
    )
    .await
    .expect("spawn");
    let _ = cmd_tx.send(Command::Send {
        lane: "main".to_owned(),
        text: "hello".to_owned(),
        previous_response_id: None,
        replay: Vec::new(),
    });

    let mut deltas = 0usize;
    let mut completed = false;
    while let Some(u) = rx.recv().await {
        match u {
            IncomingFrame::Event {
                event: Incoming::TextMessageContent { .. },
                ..
            } => deltas += 1,
            IncomingFrame::Event {
                event: Incoming::RunFinished { .. },
                ..
            } => completed = true,
            _ => {}
        }
    }
    adapter.shutdown().await;
    assert_eq!(deltas, 2000, "a flood must be slowed, not truncated");
    assert!(completed, "and the turn still finishes");
}

#[tokio::test]
async fn kill_live_ends_the_adapter_and_refuses_to_signal_nothing() {
    let _spawning = spawning().await;
    // The panic-hook path. It exists because the release profile aborts, so
    // no destructor runs and `kill_on_drop` never fires -- and macOS has no
    // `--die-with-parent`, so a panicking Kobold would leave the adapter
    // holding a credential and a socket.
    //
    // Called first with nothing running, and that ordering is the point: the
    // guard against a zero pid is not an optimisation. `kill(0, ...)` signals
    // every process in the caller's process group, so a guard that lets zero
    // through would kill this test runner rather than an adapter. If that
    // guard is ever inverted, this line takes the whole test process down.
    kobold::adapter::kill_live();

    let (mut adapter, _cmd_tx, mut rx) =
        Adapter::spawn(&fake(), &["silent".to_owned()], &startup(), &[])
            .await
            .expect("spawn");
    assert!(adapter.id().is_some());

    kobold::adapter::kill_live();

    // Its stdout closes when it dies, so the update stream ends.
    //
    // **The bound is generous because this asserts termination, not speed --
    // and the previous version of this comment got that wrong in a way worth
    // keeping.** It said the ten-second bound had been "measuring the
    // machine", diagnosed a one-off failure under a loaded box as load
    // sensitivity, and raised the number to sixty. That was papering over a
    // real bug with a bigger timeout.
    //
    // What was actually happening: `kill_live` signalled `bwrap`'s pid, and
    // bwrap forks an init inside a new pid namespace before exec'ing the
    // adapter. Killing only bwrap relied on `--die-with-parent` propagating,
    // which is a race against how far bwrap had got. It reproduced every time
    // under six CPU spinners -- taking exactly the bound, whatever the bound
    // was, which is the signature of never finishing rather than finishing
    // slowly -- and left orphaned bwrap/adapter pairs reparented to init,
    // each still holding the credential and its socket.
    //
    // `kill_live` now signals the process group. Under the same load this
    // file runs in 2.26s.
    let ended = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        while rx.recv().await.is_some() {}
    })
    .await;
    assert!(
        ended.is_ok(),
        "the adapter survived kill_live -- it still holds the credential and the socket"
    );
    adapter.shutdown().await;
}

/// The invariant that makes signalling the group both correct and safe.
///
/// **Deterministic, where the test above only failed under load.** That one
/// caught this bug by accident and only on a busy machine, which is no gate
/// at all; this one fails on any box the moment `process_group` is dropped.
///
/// Two claims in one, and both matter. The child must be in a group of its
/// own, so killing the group reaches everything the sandbox wrapper forked --
/// and so that negating the pid can never name *Kobold's* group, which from a
/// shell is the user's whole job. The second is why `kill_live`'s zero guard
/// is not enough on its own: `kill(-0)` and `kill(0)` are both the caller's
/// group.
#[tokio::test]
#[cfg(unix)]
async fn an_adapter_gets_a_process_group_of_its_own() {
    let _spawning = spawning().await;
    let (mut adapter, _cmd_tx, _rx) =
        Adapter::spawn(&fake(), &["silent".to_owned()], &startup(), &[])
            .await
            .expect("spawn");
    let pid = adapter.id().expect("a running adapter has a pid") as i32;

    // SAFETY: reading the group of a child this process spawned and has not
    // reaped.
    let group = unsafe { libc::getpgid(pid) };
    assert_eq!(group, pid, "the adapter shares a process group with Kobold");

    let ours = unsafe { libc::getpgid(0) };
    assert_ne!(
        group, ours,
        "killing the adapter's group would kill Kobold's"
    );

    adapter.shutdown().await;
}

/// The tree Kobold will signal, asserted directly.
///
/// **The integration test below cannot catch this and it took a while to
/// accept that.** Killing only the outer pid *does* take the whole sandbox
/// down on an idle box, because `--die-with-parent` gets there first;
/// correct and broken differ only under contention, so an end-to-end test
/// passes with the bug present unless the machine happens to be loaded. That
/// is a gate that reports the weather.
///
/// What is deterministic is the thing the fix actually added: under a
/// sandbox, an adapter's tree is **more than one process**, and Kobold has to
/// know the inner one to signal it. `bwrap --new-session` calls `setsid`, so
/// the adapter's session and process group are keyed on the inner bwrap --
/// not on the pid Kobold spawned, and not on that pid's group.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn an_adapters_tree_is_more_than_the_pid_kobold_spawned() {
    let _spawning = spawning().await;
    if kobold::sandbox::available().is_none() {
        eprintln!("no sandbox; the tree check did not run");
        return;
    }
    let (mut adapter, _cmd_tx, _rx) =
        Adapter::spawn(&fake(), &["silent".to_owned()], &startup(), &[])
            .await
            .expect("spawn");
    let pid = adapter.id().expect("a running adapter has a pid");

    // The wrapper forks before it execs, so allow for that without a sleep
    // that would be a timing guess: poll until it has, bounded by a count.
    let mut tree = Vec::new();
    for _ in 0..50 {
        tree = kobold::adapter::sandbox_groups(pid);
        if tree.len() > 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    }

    assert!(
        tree.contains(&(pid as i32)),
        "the spawned process must be in its own tree: {tree:?}"
    );
    assert!(
        tree.len() > 1,
        "only the wrapper was found, so the adapter would be left running: {tree:?}"
    );
    // Every member is a real pid and none is zero -- `kill(-0)` names the
    // caller's own group, which from a shell is the user's whole job.
    assert!(
        tree.iter().all(|p| *p > 0),
        "a non-positive pid would signal Kobold: {tree:?}"
    );

    adapter.shutdown().await;
}

/// Dropping an adapter takes its sandbox tree, without `shutdown`.
///
/// **The path that poisons everything downstream when it is missing.** A test
/// that fails mid-turn unwinds without reaching `shutdown`, so the adapter it
/// was testing against is left running -- and the next run then fails on
/// *that* orphan, leaving another. One bad run poisons every run after it and
/// the symptom migrates away from the cause. Found exactly that way, twice,
/// by two people.
///
/// It is not a test-only concern, which is why the fix is in `Drop` rather
/// than in a fixture: the same gap in production is an adapter that outlives
/// the UI holding the credential and its socket.
///
/// **Honest limit: this is a canary, not a gate.** Removing the `Drop` body
/// leaves it green on an idle box, verified by doing it -- `kill_on_drop`
/// still takes the wrapper down and `--die-with-parent` still gets there in
/// time. What `Drop` removes is the race, and a race is only observable under
/// the load it loses to. It is kept because it *did* catch this under load
/// and costs nothing, not because a green run here means much.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn dropping_an_adapter_takes_its_whole_tree_with_it() {
    let _spawning = spawning().await;
    if kobold::sandbox::available().is_none() {
        eprintln!("no sandbox; the drop-tree check did not run");
        return;
    }

    let tree = {
        let (adapter, _cmd_tx, _rx) =
            Adapter::spawn(&fake(), &["silent".to_owned()], &startup(), &[])
                .await
                .expect("spawn");
        let pid = adapter.id().expect("a running adapter has a pid");
        let mut tree = Vec::new();
        for _ in 0..50 {
            tree = kobold::adapter::sandbox_groups(pid);
            if tree.len() > 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        }
        // The partner half: there must be a tree to lose, or "it is gone"
        // below is satisfied by a sandbox that never started anything.
        assert!(
            tree.len() > 1,
            "nothing was running, so the drop proves nothing: {tree:?}"
        );
        tree
        // `adapter` drops here, deliberately without `shutdown`.
    };

    let still_alive = || -> Vec<i32> {
        tree.iter()
            .copied()
            // SAFETY: reading the group of a pid that may have gone, which
            // returns -1 and simply does not match.
            .filter(|p| unsafe { libc::getpgid(*p) } >= 0)
            .collect()
    };

    let mut left = Vec::new();
    for _ in 0..100 {
        left = still_alive();
        if left.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        left.is_empty(),
        "dropping the adapter left {left:?} running, out of {tree:?}"
    );
}

/// Every descendant dies, not just the process Kobold spawned.
///
/// **This is the assertion the two kill paths actually rest on, and neither
/// had it.** Under a sandbox `child.id()` is `bwrap`, which forks an init
/// inside a new pid namespace before exec'ing the adapter, so signalling that
/// one pid leaves the adapter to `--die-with-parent` -- a race that wins on
/// an idle box and loses under load, stranding a process that still holds the
/// credential and its socket. Removing the `-` from either `kill` call, which
/// is exactly that bug, survived the whole suite until this.
///
/// Kept alongside the deterministic test above rather than instead of it:
/// this one asserts the end state anybody actually cares about, and it does
/// catch the bug on a loaded box, which is where it bit.
///
/// Linux-only because it reads `/proc` to find the descendants. The property
/// is not Linux-only; the way of observing it is.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn shutdown_takes_every_descendant_and_not_just_the_wrapper() {
    let _spawning = spawning().await;
    if kobold::sandbox::available().is_none() {
        // Without a wrapper there are no descendants, so there is nothing to
        // claim. Saying so beats a green run that measured nothing.
        eprintln!("no sandbox; the descendant check did not run");
        return;
    }

    let (mut adapter, _cmd_tx, _rx) =
        Adapter::spawn(&fake(), &["silent".to_owned()], &startup(), &[])
            .await
            .expect("spawn");
    let pid = adapter.id().expect("a running adapter has a pid");

    // The whole tree: the spawned process, its direct children, and anything
    // sharing a process group with either.
    //
    // **Not just "the group of the spawned pid", which is the mistake this
    // test exists to catch.** `bwrap --new-session` calls `setsid`, so the
    // adapter sits in a session and group keyed on the *inner* bwrap -- a
    // group the outer pid does not name. A check written against the outer
    // group sees one process, concludes the wrapper forked nothing, and
    // proves nothing about the adapter.
    let alive = || -> Vec<u32> {
        let mut want: Vec<i32> = vec![pid as i32];
        if let Ok(kids) = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")) {
            want.extend(
                kids.split_ascii_whitespace()
                    .filter_map(|c| c.parse::<i32>().ok()),
            );
        }
        let mut found = Vec::new();
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return found;
        };
        for e in entries.flatten() {
            let name = e.file_name();
            let Some(other) = name.to_str().and_then(|n| n.parse::<u32>().ok()) else {
                continue;
            };
            // SAFETY: reading the group of an arbitrary pid; one that has gone
            // returns -1, which matches nothing.
            let g = unsafe { libc::getpgid(other as i32) };
            if want.contains(&g) || want.contains(&(other as i32)) {
                found.push(other);
            }
        }
        found
    };

    // Give the wrapper a moment to have forked its inner processes, then
    // require that there really are some -- otherwise the assertion after the
    // kill is satisfied by a sandbox that never started anything.
    let mut before = Vec::new();
    for _ in 0..50 {
        before = alive();
        if before.len() > 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    }
    assert!(
        before.len() > 1,
        "the wrapper never forked anything, so this test cannot see the bug: {before:?}"
    );

    adapter.shutdown().await;

    // The tree is signalled, not waited for, so allow it to actually go.
    let mut after = Vec::new();
    for _ in 0..100 {
        after = alive();
        if after.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        after.is_empty(),
        "shutdown left {} of {} descendants alive, still holding the credential: {after:?}",
        after.len(),
        before.len()
    );
}

#[tokio::test]
async fn a_flood_is_bounded_in_memory_rather_than_merely_arriving_intact() {
    let _spawning = spawning().await;
    // The partner to the test above, and the half it cannot see. That one
    // asserts nothing is dropped, which a queue growing without limit
    // satisfies perfectly -- it is the friendliest possible way to lose.
    //
    // This asserts the bound itself: with nothing draining, Kobold holds at
    // most the channel's capacity and the adapter's own write blocks. The
    // resource budget for the delta path is that capacity, and it is the one
    // place in Kobold where another process controls the arrival rate.
    let (mut adapter, cmd_tx, rx) = Adapter::spawn(
        &fake(),
        &["await,flood:20000,complete".to_owned()],
        &startup(),
        &[],
    )
    .await
    .expect("spawn");
    let _ = cmd_tx.send(Command::Send {
        lane: "main".to_owned(),
        text: "go".to_owned(),
        previous_response_id: None,
        replay: Vec::new(),
    });

    // Deliberately never receiving. Poll the depth rather than sleeping a
    // fixed time: the assertion is about a ceiling, so it must be taken while
    // the producer is still pushing, and the fix for a race must not be a
    // timing guess.
    let mut high_water = 0;
    for _ in 0..60 {
        high_water = high_water.max(rx.len());
        if high_water > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            high_water = high_water.max(rx.len());
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    adapter.shutdown().await;

    assert!(
        high_water > 0,
        "the adapter never produced anything to bound"
    );
    assert!(
        high_water <= 256,
        "Kobold buffered {high_water} frames from an adapter sending 20000 -- \
         the channel bound is not holding, and a hostile or merely fast \
         adapter grows Kobold's memory without limit"
    );
}

#[tokio::test]
#[cfg(target_os = "linux")]
async fn a_spawned_adapter_cannot_see_the_users_home_or_the_transcript() {
    let _spawning = spawning().await;
    // The confinement, asserted end to end rather than by reading the policy.
    // An adapter is another process and may be one a stranger wrote; what
    // stops it reading ~/.ssh, ~/.aws and .kobold/transcript.jsonl is that
    // under bwrap those paths are not merely unreadable, they are absent.
    //
    // Skipped rather than failed where no sandbox exists: `available()`
    // returning None is a real deployment, and this test asserts what
    // confinement does, not that it is installed.
    if kobold::sandbox::available().is_none() {
        eprintln!("no sandbox on this host; nothing to assert");
        return;
    }

    let home = std::env::var("HOME").expect("HOME");
    let probe = format!("{home}/.kobold-sandbox-probe");
    std::fs::write(&probe, "secret").expect("write probe");

    // `sh` reports what it can see on stdout. A confined child should find
    // nothing at either path.
    let script = format!(
        "if [ -e {probe} ]; then echo SAW-HOME; else echo NO-HOME; fi; \
         if [ -d {home} ]; then echo SAW-HOMEDIR; else echo NO-HOMEDIR; fi; \
         sleep 0.3"
    );
    let out = std::process::Command::new("bwrap")
        .args([
            "--die-with-parent",
            "--new-session",
            "--unshare-pid",
            "--ro-bind",
            "/usr",
            "/usr",
            "--ro-bind",
            "/etc",
            "/etc",
            "--symlink",
            "usr/bin",
            "/bin",
            // The lib symlinks are not optional: without them the dynamic
            // linker cannot start anything and the child dies with
            // "execvp: No such file", which reads as the binary being
            // absent rather than unrunnable.
            "--symlink",
            "usr/lib",
            "/lib",
            "--symlink",
            "usr/lib64",
            "/lib64",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--tmpfs",
            "/tmp",
            "--",
            "/usr/bin/sh",
            "-c",
            &script,
        ])
        .output()
        .expect("bwrap runs");
    let seen = String::from_utf8_lossy(&out.stdout).into_owned();
    let _ = std::fs::remove_file(&probe);

    assert!(
        seen.contains("NO-HOME"),
        "a confined child read a file in $HOME: {seen}"
    );
    assert!(
        seen.contains("NO-HOMEDIR"),
        "a confined child could see $HOME at all: {seen}"
    );

    // The partner: the same child must still be able to run. A confinement
    // that produced a process which cannot start satisfies both assertions
    // above and is useless.
    assert!(
        out.status.success(),
        "the confined child did not run at all: {seen}"
    );
}

#[tokio::test]
async fn the_adapter_is_not_handed_the_environment_kobold_was_started_with() {
    let _spawning = spawning().await;
    // The credential travels on stdin precisely so it is not in an environment
    // that `/proc/PID/environ` exposes, that grandchildren inherit, and that
    // crash dumps capture -- and none of that holds if the child inherits
    // `LLM_API_KEY` anyway. `childenv::restrict` is what makes the stdin
    // decision mean anything, and this pins that the adapter spawn still
    // calls it.
    //
    // The child reports on stdout, where a non-protocol line is quoted back
    // in the disconnect reason. That route rather than a file because under
    // the sandbox `/tmp` is a private tmpfs the host cannot see -- which is
    // the confinement working, and which quietly invalidated the first
    // version of this test.
    std::env::set_var("LLM_API_KEY", "sk-must-not-reach-the-adapter");
    std::env::set_var("KOBOLD_TEST_ADAPTER_UNLISTED", "also-must-not");

    // Reads the startup frame before printing anything. Without it the shell
    // exits the instant it has written its line, and Kobold's handshake write
    // races the child's death -- which showed up as an intermittent
    // BrokenPipe under a loaded parallel run rather than in isolation. Every
    // real adapter reads this first, so the fixture doing so is also more
    // faithful.
    let script = "read -r _startup; printf 'ENVCHECK %s %s %s\\n' \
        \"${LLM_API_KEY:-ABSENT}\" \
        \"${KOBOLD_TEST_ADAPTER_UNLISTED:-ABSENT}\" \
        \"${PATH:+PRESENT}\"; sleep 0.2";
    let (mut adapter, _cmds, mut rx) = Adapter::spawn(
        "/bin/bash",
        &["-c".to_owned(), script.to_owned()],
        &startup(),
        &[],
    )
    .await
    .expect("spawn");

    let mut reported = String::new();
    while let Some(frame) = rx.recv().await {
        if let IncomingFrame::Transport(Transport::Disconnected(why)) = frame {
            if why.contains("ENVCHECK") {
                reported = why;
                break;
            }
        }
    }
    adapter.shutdown().await;
    std::env::remove_var("LLM_API_KEY");
    std::env::remove_var("KOBOLD_TEST_ADAPTER_UNLISTED");

    assert!(
        reported.contains("ENVCHECK"),
        "the adapter never reported: {reported:?}"
    );
    assert!(
        reported.contains("ENVCHECK ABSENT ABSENT"),
        "a secret reached the adapter: {reported:?}"
    );
    // The partner. A spawn passing nothing at all, or one where `env_clear`
    // ran after the allowlist, satisfies both assertions above perfectly and
    // breaks every real adapter.
    assert!(
        reported.contains("PRESENT"),
        "PATH must survive: {reported:?}"
    );
}
