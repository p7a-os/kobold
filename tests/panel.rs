//! The `ask` panel as a consumer of the crate sees it: `tools::run` /
//! `tools::decide` producing a parked `Ask`, `App` turning that into a
//! panel, and the panel resolving back to an answer -- all through the
//! public surface, the same path `main.rs` drives.
//!
//! Hermetic: no network, no API key, no server.

use kobold::app::App;
use kobold::tools::{decide, run, Call, Dispatch};

fn block<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime")
        .block_on(f)
}

fn ask_call(question: &str, options: &[&str], multiple: bool) -> Call {
    let opts: Vec<String> = options.iter().map(|o| format!("\"{o}\"")).collect();
    Call {
        id: "call_it".into(),
        name: "ask".into(),
        arguments: format!(
            r#"{{"question":"{question}","options":[{}],"multiple":{multiple}}}"#,
            opts.join(",")
        ),
    }
}

/// Runs an `ask` call through the real dispatch path and parks it, the same
/// step `main.rs::absorb` performs.
fn open(app: &mut App, call: &Call) {
    let root = std::env::temp_dir();
    let Dispatch::Park(ask) = decide(block(run(&root, call, None))) else {
        panic!("ask should have parked the turn");
    };
    app.park_ask("main", ask);
}

#[test]
fn an_ask_with_four_options_offers_five_selectable_rows() {
    // The free-text option is not conditional on how many choices came in --
    // it is always there, so four options plus it must be five rows exactly.
    let mut app = App::new("main", "b0");
    open(&mut app, &ask_call("which?", &["a", "b", "c", "d"], false));
    let panel = app.panel().expect("panel is open");
    assert_eq!(
        panel.fields.len(),
        2,
        "the options and the trailing free-text field"
    );
    assert_eq!(panel.row_count(), 5, "four options plus the free-text row");
}

#[test]
fn an_ask_with_no_options_still_renders_something_answerable() {
    // `ask()` accepts an empty `options` array; the panel must still open on
    // something the user can actually answer rather than an empty screen.
    let mut app = App::new("main", "b0");
    open(&mut app, &ask_call("what should I call it?", &[], false));
    let panel = app.panel().expect("panel is open even with no options");
    assert_eq!(panel.row_count(), 1, "just the free-text row");
}

#[test]
fn a_radio_answer_is_one_value_a_checkbox_answer_is_several() {
    let mut radio = App::new("main", "b0");
    open(&mut radio, &ask_call("pick one", &["a", "b", "c"], false));
    assert!(radio.panel_move(1), "move onto b");
    let answers = radio.submit_panel();
    assert_eq!(answers.len(), 1, "one call, one answer");
    assert_eq!(answers[0].1, "b", "exactly the one chosen");

    let mut boxes = App::new("main", "b0");
    open(&mut boxes, &ask_call("pick some", &["a", "b", "c"], true));
    assert!(boxes.panel_toggle(), "check a");
    assert!(boxes.panel_move(1));
    assert!(boxes.panel_toggle(), "check b");
    let answers = boxes.submit_panel();
    assert_eq!(answers.len(), 1, "one call, one answer");
    assert_eq!(answers[0].1, "a, b", "every box checked, joined");
}

#[test]
fn esc_declines_and_the_turn_is_no_longer_parked() {
    let mut app = App::new("main", "b0");
    open(&mut app, &ask_call("continue?", &["yes", "no"], false));
    assert!(app.ask_parked_on("main"), "the turn starts parked");

    let token = app.cancel_panel().expect("a panel was open to cancel");
    assert!(!app.panel_open(), "esc closes the panel");

    // What `main.rs::resolve_ask` sends as the tool's output on a decline.
    let (lane, ask) = app
        .resolve_pending_ask(&token)
        .expect("the token matches what was parked");
    assert_eq!(lane, "main");
    assert_eq!(ask.call_id, "call_it");
    // The question comes back too, because the caller has to put it into the
    // conversation and it is gone from the pane by then.
    assert!(
        !ask.question.is_empty(),
        "the question must survive resolution"
    );
    assert!(
        !app.ask_parked_on("main"),
        "the turn is unparked once the decline resolves"
    );
}

#[test]
fn a_mismatched_token_sends_nothing_and_leaves_the_question_pending() {
    // From outside: a stray or stale token must not resolve someone else's
    // question, and the real one must still be there to answer afterwards.
    let mut app = App::new("main", "b0");
    open(&mut app, &ask_call("continue?", &["yes"], false));

    assert!(app.resolve_pending_ask("not_the_real_call_id").is_none());
    assert!(
        app.ask_parked_on("main"),
        "the real question was not silently dropped"
    );
    assert!(
        app.panel_open(),
        "the panel is still up for the question that is actually pending"
    );

    // The real token still resolves it, proving nothing about the pending
    // question itself was disturbed by the mismatch.
    let answers = app.submit_panel();
    assert_eq!(answers.len(), 1, "panel was never touched by the mismatch");
    assert_eq!(answers[0].1, "yes");
    assert!(app.resolve_pending_ask("call_it").is_some());
}
