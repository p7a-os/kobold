//! The tool path as a consumer of the crate sees it.
//!
//! The unit tests in `src/tools.rs` reach inside; these use only what is
//! public, which is what the event loop and the MCP client actually have. A
//! tool that works internally and is unreachable through the public surface is
//! a tool that does not work.
//!
//! Hermetic: no network, no API key, no server. The one remote source here is a
//! fake, which is the point of the trait it implements.

use kobold::tools::{decide, run, Call, Dispatch, McpTools, Outcome, MAX_OPTIONS};
use std::path::{Path, PathBuf};

/// A working directory with something in it, and somewhere outside to try to
/// reach.
struct Fixture {
    tmp: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let tmp = std::env::temp_dir().join(format!("kobold-it-{tag}-{}", std::process::id()));
        let root = tmp.join("work");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::create_dir_all(tmp.join("outside")).expect("mkdir");
        std::fs::write(root.join("notes.md"), b"# notes\nvisible").expect("write");
        std::fs::write(tmp.join("outside/private.txt"), b"not yours").expect("write");
        Fixture { tmp, root }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

fn call(name: &str, arguments: &str) -> Call {
    Call {
        id: "call_it".into(),
        name: name.into(),
        arguments: arguments.into(),
    }
}

fn block<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime")
        .block_on(f)
}

#[test]
fn a_turn_that_reads_a_file_gets_its_contents_back_as_output() {
    let f = Fixture::new("read");
    let outcome = block(run(
        &f.root,
        &call("file_read", r#"{"path":"notes.md"}"#),
        None,
    ));
    assert_eq!(
        decide(outcome),
        Dispatch::Reply {
            output: "# notes\nvisible".to_owned(),
            error: false
        }
    );
}

#[test]
fn a_turn_that_reaches_outside_the_cone_is_answered_not_ended() {
    // The distinction the whole design turns on: the model gets an answer it
    // can act on, and the conversation survives.
    let f = Fixture::new("escape");
    for probe in [
        r#"{"path":"../outside/private.txt"}"#,
        r#"{"path":"/etc/passwd"}"#,
        r#"{"path":"./../../etc/hosts"}"#,
    ] {
        let outcome = block(run(&f.root, &call("file_read", probe), None));
        let Dispatch::Reply { output: text, .. } = decide(outcome) else {
            panic!("{probe} should have been answered, not parked");
        };
        assert!(
            text.contains("outside") || text.contains("cannot read"),
            "{probe} produced {text:?}"
        );
        assert!(
            !text.contains("not yours"),
            "{probe} leaked the file's contents"
        );
        assert!(!text.contains("root:"), "{probe} leaked /etc/passwd");
    }
}

#[test]
fn asking_the_user_parks_the_turn() {
    let f = Fixture::new("ask");
    let c = call(
        "ask",
        r#"{"question":"Ship it?","options":["yes","not yet"]}"#,
    );

    let Dispatch::Park(ask) = decide(block(run(&f.root, &c, None))) else {
        panic!("a free panel takes the question");
    };
    assert_eq!(ask.question, "Ship it?");
    assert_eq!(ask.options.len(), 2);
    assert_eq!(ask.call_id, "call_it", "the answer must find its way back");
    assert!(!ask.multiple, "single-select unless asked for");
}

#[test]
fn the_option_limit_is_the_same_number_the_model_is_told() {
    // Two ways to learn the bound -- the schema and a refusal -- and they have
    // to agree, or the model is misled by one of them.
    let f = Fixture::new("limit");
    let too_many: Vec<String> = (0..=MAX_OPTIONS).map(|i| format!("\"opt{i}\"")).collect();
    let args = format!(r#"{{"question":"?","options":[{}]}}"#, too_many.join(","));
    let Dispatch::Reply { output: text, .. } =
        decide(block(run(&f.root, &call("ask", &args), None)))
    else {
        panic!("more options than the panel holds should be refused");
    };
    assert!(text.contains(&MAX_OPTIONS.to_string()), "{text}");

    let (_, _, schema) = kobold::tools::schemas()
        .into_iter()
        .find(|(n, _, _)| *n == "ask")
        .expect("ask is described");
    assert!(
        schema.contains(&format!("\"maxItems\": {MAX_OPTIONS}")),
        "{schema}"
    );
}

#[test]
fn every_offered_tool_can_actually_be_called() {
    // The pairing that matters end to end: a name in the schema list that
    // dispatch does not know is a tool the model will call and never reach.
    let f = Fixture::new("offered");
    for (name, _, _) in kobold::tools::schemas() {
        let outcome = block(run(&f.root, &call(name, "{}"), None));
        // Empty arguments are wrong for both tools, so a refusal is expected.
        // What must not happen is "no tool named ...".
        if let Outcome::Refused(why) = &outcome {
            assert!(
                !why.contains("no tool named"),
                "{name} is offered to the model but cannot be dispatched"
            );
        }
    }
}

/// A remote source that answers from memory, standing in for an MCP server.
struct FakeServer;

#[async_trait::async_trait]
impl McpTools for FakeServer {
    fn owns(&self, name: &str) -> bool {
        name.starts_with("files__")
    }
    async fn call(&self, call: &Call) -> Outcome {
        Outcome::Done(format!("{} answered", call.name))
    }
    fn schemas(&self) -> Vec<(String, String, String)> {
        vec![(
            "files__grep".to_owned(),
            "search files on the server".to_owned(),
            "{}".to_owned(),
        )]
    }
}

#[test]
fn a_remote_tool_answers_and_a_local_one_cannot_be_impersonated() {
    let f = Fixture::new("remote");
    let server = FakeServer;

    let out = block(run(&f.root, &call("files__grep", "{}"), Some(&server)));
    assert_eq!(out, Outcome::Done("files__grep answered".to_owned()));

    // The security property, from outside: whatever a server offers, the local
    // tool answers for the local name.
    let out = block(run(
        &f.root,
        &call("file_read", r#"{"path":"notes.md"}"#),
        Some(&server),
    ));
    assert_eq!(out, Outcome::Done("# notes\nvisible".to_owned()));
}

#[test]
fn an_unknown_name_names_everything_that_does_exist() {
    let f = Fixture::new("unknown");
    let Outcome::Refused(why) = block(run(&f.root, &call("no_such_tool", "{}"), Some(&FakeServer)))
    else {
        panic!("an unknown name is refused");
    };
    assert!(why.contains("no_such_tool"), "{why}");
    assert!(why.contains("file_read"), "local tools missing: {why}");
    assert!(why.contains("files__grep"), "remote tools missing: {why}");
}

#[test]
fn the_cone_is_measured_from_the_directory_it_is_given() {
    // Two fixtures, so a path that is inside one is outside the other. If the
    // cone came from anywhere but the argument, this would not hold.
    let a = Fixture::new("cone-a");
    let b = Fixture::new("cone-b");
    let target = a.root.join("notes.md");
    let args = format!(r#"{{"path":"{}"}}"#, target.display());

    assert!(matches!(
        block(run(&a.root, &call("file_read", &args), None)),
        Outcome::Done(_)
    ));
    let Outcome::Refused(why) = block(run(&b.root, &call("file_read", &args), None)) else {
        panic!("a's file is outside b's cone");
    };
    assert!(why.contains("outside"), "{why}");
    let _ = Path::new("");
}
