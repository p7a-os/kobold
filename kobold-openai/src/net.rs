//! Socket task: owns the connection, converts borrowed wire events into owned
//! updates for the UI.
//!
//! The UI cannot hold borrowed events -- they live only as long as the read
//! buffer -- so this is the one place where allocation is accepted on the event
//! path. It costs one small `String` per delta, which measured against a
//! network-bound workload (CPU time is below the 10ms floor) is free.

use tokio::sync::mpsc;

use crate::events::{Event, ResponseCreate};
use crate::json;
use crate::ws::{self};

pub use kobold_proto::agui::{Base, Outgoing};
pub use kobold_proto::{Command, Model, OutgoingFrame, Startup, Transport};

/// Why a connection ended, which is the only thing that decides what happens
/// next.
#[derive(Debug, PartialEq)]
pub enum Ended {
    /// Kobold said to stop, or its command channel closed. Nothing to retry.
    Shutdown,
    /// The socket died for a reason that a fresh one might not have.
    Lost(String),
    /// The socket died for a reason a fresh one would hit identically.
    /// Retrying a rejected credential forever is a worse failure than saying
    /// so once and stopping.
    Fatal(String),
    /// Nothing is wrong; this connection has simply been up long enough that
    /// the server is about to close it.
    Rotate,
}

/// Whether it is worth connecting again.
///
/// **The default is to retry, and the exceptions are named.** A transport
/// error is usually the network, and the network usually comes back; an
/// unrecognised failure is far more likely to be a blip than a permanent
/// rejection, so treating the unknown as fatal would turn a dropped packet
/// into a dead session.
///
/// Matched on the rendered message rather than on a typed status, because
/// that is what the transport gives us: `ws::ConnectError` renders a rejected
/// handshake as `websocket: Invalid status code: 401`, and there is no status
/// field to read instead. Fragile in principle; the alternative is retrying a
/// bad key every few seconds until someone notices.
fn is_fatal(reason: &str) -> bool {
    // 401 is a rejected key and 403 a key without access to the model. Both
    // are answered by editing configuration, never by waiting.
    reason.contains("401") || reason.contains("403")
}

/// How long to wait before attempt `n`, capped.
///
/// Doubling from a second, ceiling of thirty. The ceiling matters more than
/// the curve: an outage lasting an hour must not become an hour of
/// exponentially rarer attempts, and it must not become an hour of hammering
/// the provider with the user's key either.
fn backoff(attempt: u32) -> std::time::Duration {
    const CAP: u64 = 30;
    std::time::Duration::from_secs((1u64 << attempt.min(5)).min(CAP))
}

/// Connect, run the loop on that socket, and connect again when it ends for a
/// reason a new socket would fix.
///
/// **Built for the pessimistic answer to a question that is still open**: a
/// `previous_response_id` is assumed *not* to survive a new connection, so
/// `run_on` starts each socket with an empty map and continuations fall back
/// to the replay path that `Command::Send` already carries. Replay costs more
/// -- it re-sends the context and the user pays for it again -- but it is
/// never *wrong*, where the optimistic build is wrong exactly when the
/// assumption fails, and silently, as context the model no longer has. If the
/// id turns out to survive, keeping the map across the boundary is the whole
/// change.
///
/// Kobold needs nothing new for this. `Disconnected` already abandons every
/// outstanding request, so a reconnect starts with no leaked counts, and
/// `Connected` already puts the panes back up.
/// How the reconnect loop gets a socket.
///
/// **Exists so the loop can be tested, and that is not a small thing: the
/// loop is where every reconnect *decision* lives** -- what to retry, how
/// long to wait, when to give up, when to reset the attempt count. With
/// connecting hard-coded, all of that needed a network to exercise, and a
/// mutation run said so plainly: `run` replaced with an empty body survived
/// the entire suite, because nothing reached it.
pub trait Connect {
    type Conn: ws::Transport;
    /// `Err` carries the reason as it would be shown, which is what
    /// `is_fatal` reads.
    fn connect(&mut self) -> impl std::future::Future<Output = Result<Self::Conn, String>> + Send;
}

/// The real one: a WebSocket to the provider.
///
/// Public, and constructed by `main` rather than by a `run` wrapper here.
/// There used to be such a wrapper -- one line, `run_with(Dial(key), ..)` --
/// and it was the last thing in this file no test could reach: exercising it
/// needs a live socket, so a mutation replacing its body with nothing
/// survived every run. Deleting it is the honest fix rather than excusing it,
/// and it leaves the reconnect policy in `run_with`, where it is driven by a
/// scripted `Connect` and every decision in it is covered.
pub struct Dial {
    pub api_key: String,
    /// Kobold's egress broker, as a path inside the sandbox, or `None` for a
    /// direct connection. Re-read on every attempt rather than resolved once:
    /// a reconnect is a fresh request through the broker, and the broker gets
    /// to refuse it again.
    pub egress: Option<String>,
}

impl Connect for Dial {
    type Conn = ws::Stream;
    async fn connect(&mut self) -> Result<Self::Conn, String> {
        match self.egress.as_deref() {
            Some(socket) => ws::connect_via(&self.api_key, socket).await,
            None => ws::connect(&self.api_key).await,
        }
        .map_err(|e| e.to_string())
    }
}

/// Connect, run the loop on that socket, and connect again when it ends for a
/// reason a new socket would fix.
///
/// **Built for the pessimistic answer to a question that is still open**: a
/// `previous_response_id` is assumed *not* to survive a new connection, so
/// `run_on` starts each socket with an empty map and continuations fall back
/// to the replay path that `Command::Send` already carries. Replay costs more
/// -- it re-sends the context and the user pays for it again -- but it is
/// never *wrong*, where the optimistic build is wrong exactly when the
/// assumption fails, and silently, as context the model no longer has. If the
/// id turns out to survive, keeping the map across the boundary is the whole
/// change.
///
/// Kobold needs nothing new for this. `Disconnected` already abandons every
/// outstanding request, so a reconnect starts with no leaked counts, and
/// `Connected` already puts the panes back up.
pub async fn run_with<C: Connect>(
    mut dial: C,
    model: Model,
    tx: mpsc::UnboundedSender<OutgoingFrame>,
    mut rx: mpsc::UnboundedReceiver<Command>,
) {
    let mut attempt: u32 = 0;
    loop {
        let conn = match dial.connect().await {
            Ok(c) => c,
            Err(reason) => {
                let _ = tx.send(OutgoingFrame::Transport(Transport::Disconnected(
                    reason.clone(),
                )));
                if is_fatal(&reason) {
                    return;
                }
                // A failure to connect counts as an attempt; otherwise a
                // provider that refuses TCP would be retried in a tight loop.
                tokio::time::sleep(backoff(attempt)).await;
                attempt = attempt.saturating_add(1);
                continue;
            }
        };
        // Reset here rather than after `run_on`: what proves the provider is
        // reachable is a socket that opened, not one that lasted. A
        // connection that dies immediately and repeatedly still backs off.
        attempt = 0;
        let _ = tx.send(OutgoingFrame::Transport(Transport::Connected));

        match run_on(conn, &model, &tx, &mut rx).await {
            Ended::Shutdown | Ended::Fatal(_) => return,
            // Not announced as a disconnection, because nothing failed and
            // the user has no decision to make about it. `run_on` only
            // returns this in an idle gap, so there is no turn to interrupt
            // and nothing on screen to correct.
            Ended::Rotate => continue,
            Ended::Lost(_) => {
                // `run_on` has already sent the reason; this is only the
                // pause before trying again.
                tokio::time::sleep(backoff(attempt)).await;
                attempt = attempt.saturating_add(1);
            }
        }
    }
}

/// The loop itself, over anything that can carry frames.
///
/// Split from `run` so it can be driven by a scripted transport. Connecting
/// is the only part that needs a network, and it is the part with no logic in
/// it; everything that decides what an event means is here.
pub async fn run_on<T: ws::Transport>(
    mut conn: T,
    model: &Model,
    tx: &mpsc::UnboundedSender<OutgoingFrame>,
    rx: &mut mpsc::UnboundedReceiver<Command>,
) -> Ended {
    // Parsed up front so a broken schema is a startup failure rather than a
    // rejected request in the middle of a conversation.
    //
    // One list, whatever the tool actually is. A tool Kobold runs locally and
    // one it reaches over MCP are the same thing to the provider -- a name, a
    // description and a schema -- and both come back as the same call. Which
    // is which is Kobold's business, and keeping that distinction out of here
    // is what lets MCP stay entirely on its side.
    let local_tools: Vec<(&str, &str, sonic_rs::Value)> = model
        .tools
        .iter()
        .filter_map(
            |(name, description, schema)| match sonic_rs::from_str(schema) {
                Ok(v) => Some((name.as_str(), description.as_str(), v)),
                Err(e) => {
                    let _ = tx.send(OutgoingFrame::Transport(Transport::Disconnected(format!(
                        "tool '{name}' has a bad schema: {e}"
                    ))));
                    None
                }
            },
        )
        .collect();

    // Built once. The set is fixed for the process, and rebuilding it per
    // request would re-serialise the same schemas for every message sent.
    let mut offered: Vec<crate::events::ToolDef> = local_tools
        .iter()
        .map(|(name, description, parameters)| {
            crate::events::ToolDef::function(name, description, parameters.clone())
        })
        .collect();
    // Server-side tools are named and nothing more: the API runs them, so
    // there is no schema to give and no call that comes back to us.
    offered.extend(
        model
            .server_tools
            .iter()
            .map(|t| crate::events::ToolDef::server(t)),
    );
    // The response each lane is currently producing. A tool result has to be
    // attached to the response that asked for it, and that id is only ever
    // seen here, on the events -- the UI learns it when a turn completes,
    // which is too late to answer a call made during one.
    let mut in_flight: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    // How many responses have been asked for and not yet ended. Rotation
    // waits for this to reach zero.
    //
    // **Not `in_flight.is_empty()`, which is the trap here**: that map is
    // inserted into by every event naming a response and never cleared, so
    // after the first turn of a session it is permanently non-empty and an
    // idle check built on it would never fire -- or, read the other way,
    // would rotate under a live turn if someone "fixed" it by clearing on
    // completion. It answers "which response should a tool result attach
    // to", which is a different question from "is anything happening".
    let mut outstanding: usize = 0;
    let rotate_at = tokio::time::Instant::now() + ws::ROTATE_AFTER;

    loop {
        tokio::select! {
            // Only in an idle gap. The guard disables the branch while a turn
            // is running, so the deadline passing mid-turn does not drop it:
            // the branch simply becomes ready the moment the last response
            // ends. Rotating under a live turn would throw away output the
            // user has already been billed for.
            () = tokio::time::sleep_until(rotate_at), if outstanding == 0 => {
                return Ended::Rotate;
            }
            cmd = rx.recv() => match cmd {
                Some(Command::Send { lane, text, previous_response_id, replay }) => {
                    let mut create =
                        ResponseCreate::user_text(&model.name, Some(&lane), &text, &model.effort);
                    create.tools = Some(&offered);
                    create.previous_response_id = previous_response_id.as_deref();
                    if previous_response_id.is_none() && !replay.is_empty() {
                        let mut input: Vec<_> = replay
                            .iter()
                            .map(|(role, text)| crate::events::InputItem::message(role, text))
                            .collect();
                        input.append(&mut create.input);
                        create.input = input;
                    }
                    let body = match json::to_string(&create) {
                        Ok(b) => b,
                        Err(e) => { let _ = tx.send(OutgoingFrame::Transport(Transport::Disconnected(e.to_string()))); return Ended::Fatal(e.to_string()); }
                    };
                    if let Err(e) = conn.send_text(&body).await {
                        let _ = tx.send(OutgoingFrame::Transport(Transport::Disconnected(e.to_string())));
                        return Ended::Lost(e.to_string());
                    }
                    outstanding = outstanding.saturating_add(1);
                }
                // `error` is read and deliberately dropped, which is the
                // honest thing for this provider rather than an oversight.
                // The Responses API's `function_call_output` carries a
                // `call_id` and an `output` string and nothing else in the
                // shape we send -- **whether it accepts a failure marker at
                // all is UNVERIFIED**, since settling it needs the live API.
                // Inventing a prefix convention would be putting a string
                // the model has to guess the meaning of where a field
                // belongs, so the flag waits for a provider with somewhere
                // to put it. A conforming AG-UI adapter has `ToolMessage.error`.
                Some(Command::ToolResult { lane, call_id, output, error: _ }) => {
                    let mut create = ResponseCreate::user_text(&model.name, Some(&lane), "", &model.effort);
                    // The output replaces the message entirely: this turn is a
                    // continuation of one the model is already in the middle
                    // of, not a new thing to say.
                    create.tools = Some(&offered);
                    create.input = vec![crate::events::InputItem::tool_output(&call_id, output)];
                    create.previous_response_id = in_flight.get(&lane).map(String::as_str);
                    let body = match json::to_string(&create) {
                        Ok(b) => b,
                        Err(e) => { let _ = tx.send(OutgoingFrame::Transport(Transport::Disconnected(e.to_string()))); return Ended::Fatal(e.to_string()); }
                    };
                    if let Err(e) = conn.send_text(&body).await {
                        let _ = tx.send(OutgoingFrame::Transport(Transport::Disconnected(e.to_string())));
                        return Ended::Lost(e.to_string());
                    }
                    outstanding = outstanding.saturating_add(1);
                }
                Some(Command::Cancel { lane }) => {
                    in_flight.remove(&lane);
                    outstanding = outstanding.saturating_sub(1);
                }
                Some(Command::Quit) | None => return Ended::Shutdown,
            },
            read = conn.read() => {
                let msg = match read {
                    Ok(ws::Incoming::Text(b)) => b,
                    Ok(ws::Incoming::Closed) => {
                        let _ = tx.send(OutgoingFrame::Transport(Transport::Disconnected("server closed the connection".into())));
                        return Ended::Lost("server closed the connection".to_owned());
                    }
                    Ok(ws::Incoming::Other) => continue,
                    Err(e) => {
                        let _ = tx.send(OutgoingFrame::Transport(Transport::Disconnected(e.to_string())));
                        return Ended::Lost(e.to_string());
                    }
                };
                // An unmodelled or malformed event must never kill the session.
                let Ok(ev) = json::from_slice::<Event>(msg.as_ref()) else { continue };
                // Every event that names a response updates the record, so a
                // tool call arriving mid-turn can be answered against the
                // response that made it.
                if let Some(r) = ev.response.as_ref() {
                    in_flight.insert(ev.stream_id.unwrap_or_default().to_owned(), r.id.to_owned());
                }
                // Decremented here rather than on the frames, because a
                // terminal event is exactly one response ending whether it
                // becomes a `RunFinished` or a `RunError`.
                if ev.is_terminal() {
                    outstanding = outstanding.saturating_sub(1);
                }
                for frame in to_frames(&ev, &model.name) {
                    if tx.send(frame).is_err() {
                        // Kobold is gone. Reconnecting to serve nobody would
                        // keep a billable socket open after the UI exited.
                        return Ended::Shutdown;
                    }
                }
            }
        }
    }
}

/// The message id for one reasoning summary part of one lane.
///
/// Distinct from the lane itself, which is what a text message uses: a
/// reasoning message and the answer that follows it are different messages,
/// and giving them one id would make the summary and the reply the same
/// stream of text to anything keying on the id.
fn reasoning_id(lane: &str, summary_index: Option<u32>) -> String {
    format!("{lane}:reasoning:{}", summary_index.unwrap_or(0))
}

/// One provider event as the AG-UI frames it becomes.
///
/// A `Vec` because one provider event can be several: a tool call arrives
/// complete in `response.output_item.done`, where AG-UI wants
/// `TOOL_CALL_START`, `TOOL_CALL_ARGS` and `TOOL_CALL_END`. **Emitting all
/// three rather than only the start is what lets Kobold close the call
/// without knowing this provider's shape**, which is the whole point of the
/// adapter.
///
/// `model` is threaded in for one reason: AG-UI's `TokenUsage` names the
/// model per entry and the provider's usage object does not.
fn to_frames(ev: &Event<'_>, model: &str) -> Vec<OutgoingFrame> {
    let lane = ev.stream_id.unwrap_or_default().to_owned();
    let mut out = Vec::new();
    let wrap = |event| OutgoingFrame::Event {
        lane: lane.clone(),
        event,
    };

    match ev.kind {
        "response.output_text.delta" => {
            if let Some(d) = ev.delta.as_deref() {
                // No `TEXT_MESSAGE_START` before it and no `TEXT_MESSAGE_END`
                // after, deliberately: this provider gives no message id and
                // no boundary this adapter could honestly derive one from.
                // Inventing a pair would be inventing structure the wire does
                // not carry, and Kobold's transcript keys off the lane rather
                // than the message. Revisit when a provider supplies one.
                out.push(wrap(Outgoing::TextMessageContent {
                    base: Base::default(),
                    message_id: lane.clone(),
                    delta: d.to_owned(),
                }));
            }
        }
        // Reasoning summaries, which are what turn a long think from silence
        // into something on screen.
        //
        // **Kobold had never asked for these.** The request carried
        // `reasoning.effort` and no `summary`, so the provider had no reason
        // to send them -- which is a complete explanation of the 435 seconds
        // of measured silence before a first delta, without the provider
        // being incapable of it. `ResponseCreate` now asks.
        //
        // Keyed per summary part, not per turn: `summary_index` is on every
        // one of these events because a turn can produce several parts, and
        // folding them onto one id would run them together.
        "response.reasoning_summary_part.added" => {
            out.push(wrap(Outgoing::ReasoningMessageStart {
                base: Base::default(),
                message_id: reasoning_id(&lane, ev.summary_index),
                role: "assistant".to_owned(),
            }));
        }
        "response.reasoning_summary_text.delta" => {
            if let Some(d) = ev.delta.as_deref() {
                out.push(wrap(Outgoing::ReasoningMessageContent {
                    base: Base::default(),
                    message_id: reasoning_id(&lane, ev.summary_index),
                    delta: d.to_owned(),
                }));
            }
        }
        // `part.done` rather than `text.done`, which arrives first and says
        // the text of this part is complete rather than the part itself. Both
        // would close it; emitting on both would close it twice.
        //
        // **`REASONING_START` and `REASONING_END` are deliberately not
        // emitted**, and it is the same call already made for
        // `TEXT_MESSAGE_START`/`END`: they would have to be derived from a
        // reasoning-item boundary this adapter has never seen the payload of.
        // The summary events above were verified against a real stream; that
        // boundary was not, and inventing it would be inventing structure the
        // wire may not carry. Kobold does not need them -- what it needed was
        // for the silence to stop being silent, and the deltas do that.
        "response.reasoning_summary_part.done" => {
            out.push(wrap(Outgoing::ReasoningMessageEnd {
                base: Base::default(),
                message_id: reasoning_id(&lane, ev.summary_index),
            }));
        }
        // Not `response.function_call_arguments.done`, which arrives first and
        // carries the arguments without saying which tool they are for. This
        // one has the name, the call id and the arguments together, so a call
        // is dispatched from one event rather than stitched from two.
        "response.output_item.done" => {
            if let Some(item) = ev.item.as_ref().filter(|i| i.kind == "function_call") {
                let id = item.call_id.unwrap_or_default().to_owned();
                out.push(wrap(Outgoing::ToolCallStart {
                    base: Base::default(),
                    tool_call_id: id.clone(),
                    tool_call_name: item.name.unwrap_or_default().to_owned(),
                    parent_message_id: None,
                }));
                out.push(wrap(Outgoing::ToolCallArgs {
                    base: Base::default(),
                    tool_call_id: id.clone(),
                    delta: item.arguments.as_deref().unwrap_or_default().to_owned(),
                }));
                out.push(wrap(Outgoing::ToolCallEnd {
                    base: Base::default(),
                    tool_call_id: id,
                }));
            }
        }
        _ => {}
    }

    if ev.is_failure() {
        let err = ev.error.as_ref();
        out.push(wrap(Outgoing::RunError {
            base: Base::default(),
            // Required by the specification where ours was optional, so a
            // missing one becomes a sentence rather than an empty string:
            // "failed:" with nothing after it tells the user nothing.
            message: err
                .and_then(|e| e.message.as_deref())
                .unwrap_or("the provider failed the turn without saying why")
                .to_owned(),
            code: err.and_then(|e| e.code).map(str::to_owned),
            usage: ev
                .response
                .as_ref()
                .and_then(|r| r.usage)
                .map(|u| vec![u.to_agui(model)]),
        }));
    } else if ev.is_terminal() {
        out.push(wrap(Outgoing::RunFinished {
            base: Base::default(),
            thread_id: lane.clone(),
            // The provider's response id *is* the run id, which is what makes
            // `previous_response_id` still expressible: `resp_...` on the
            // wire, observed.
            run_id: ev
                .response
                .as_ref()
                .map(|r| r.id.to_owned())
                .unwrap_or_default(),
            // Fold A, and the reason it lives here: only this crate knows
            // that `reasoning_tokens` is nested under `output_tokens_details`.
            usage: ev
                .response
                .as_ref()
                .and_then(|r| r.usage)
                .map(|u| vec![u.to_agui(model)]),
        }));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Events are parsed from real wire JSON rather than built as structs.
    /// `Event` borrows out of the buffer it is parsed from, so constructing
    /// one by hand would test a shape the provider never actually sends --
    /// and the field names and nesting are exactly what would drift.
    fn frames_for(raw: &str) -> Vec<OutgoingFrame> {
        let ev: Event = crate::json::from_slice(raw.as_bytes()).expect("parse the event");
        to_frames(&ev, "test-model")
    }

    /// A frame reduced to `(lane, what happened)`.
    ///
    /// Spelling out whole `Outgoing` values would mean an empty `Base` on
    /// every row and would still not check the one thing worth checking
    /// exactly, which is usage -- so that has its own assertion below and
    /// everything else compares against a sentence.
    fn summary(f: &OutgoingFrame) -> (String, String) {
        match f {
            OutgoingFrame::Transport(Transport::Connected) => (String::new(), "connected".into()),
            OutgoingFrame::Transport(Transport::Disconnected(why)) => {
                (String::new(), format!("disconnected:{why}"))
            }
            OutgoingFrame::Event { lane, event } => (
                lane.clone(),
                match event {
                    Outgoing::TextMessageContent { delta, .. } => format!("delta:{delta}"),
                    Outgoing::ToolCallStart {
                        tool_call_id,
                        tool_call_name,
                        ..
                    } => {
                        format!("tool_start:{tool_call_id}:{tool_call_name}")
                    }
                    Outgoing::ToolCallArgs {
                        tool_call_id,
                        delta,
                        ..
                    } => {
                        format!("tool_args:{tool_call_id}:{delta}")
                    }
                    Outgoing::ToolCallEnd { tool_call_id, .. } => {
                        format!("tool_end:{tool_call_id}")
                    }
                    Outgoing::RunFinished { run_id, usage, .. } => {
                        format!("finished:{run_id}:{}", usage.is_some())
                    }
                    Outgoing::RunError { code, message, .. } => {
                        format!("error:{}:{message}", code.clone().unwrap_or_default())
                    }
                    other => format!("unexpected:{other:?}"),
                },
            ),
        }
    }

    fn summaries(frames: &[OutgoingFrame]) -> Vec<(String, String)> {
        frames.iter().map(summary).collect()
    }

    fn one(lane: &str, what: &str) -> Vec<(String, String)> {
        vec![(lane.to_owned(), what.to_owned())]
    }

    #[test]
    fn a_text_delta_becomes_one_delta_on_its_own_lane() {
        let got = frames_for(
            r#"{"type":"response.output_text.delta","stream_id":"fork-1","delta":"hello"}"#,
        );
        assert_eq!(summaries(&got), one("fork-1", "delta:hello"));
    }

    #[test]
    fn an_event_with_no_stream_id_lands_on_the_default_lane() {
        // The implicit lane is absent from the wire rather than empty, and
        // Kobold's default pane is named for the empty string -- so these two
        // conventions have to agree or a single-pane session drops everything
        // it is sent.
        let got = frames_for(r#"{"type":"response.output_text.delta","delta":"hi"}"#);
        assert_eq!(summaries(&got), one("", "delta:hi"));
    }

    #[test]
    fn a_delta_carrying_escapes_survives_intact() {
        // The reason `Event::delta` is a `Cow`: an escaped delta cannot be
        // borrowed out of the read buffer, and with `&str` it fails to
        // deserialize and the text is silently lost.
        let got =
            frames_for(r#"{"type":"response.output_text.delta","delta":"line\none\t\"quoted\""}"#);
        assert_eq!(summaries(&got), one("", "delta:line\none\t\"quoted\""));
    }

    #[test]
    fn a_finished_function_call_becomes_one_tool_call() {
        // From `output_item.done` and not from
        // `function_call_arguments.done`: only this one carries the name, the
        // call id and the arguments together, so a call is dispatched from
        // one event rather than stitched from two.
        // Three AG-UI events, not one. The provider hands the whole call
        // over at once and the protocol has no event for that, so the adapter
        // emits the boundary too -- Kobold closes the call on `tool_end`
        // without knowing anything about this provider's shape, which is the
        // whole point of putting a provider behind an adapter.
        let got = frames_for(
            r#"{"type":"response.output_item.done","stream_id":"main",
                "item":{"type":"function_call","call_id":"call_1",
                        "name":"file_read","arguments":"{\"path\":\"a.txt\"}"}}"#,
        );
        assert_eq!(
            summaries(&got),
            vec![
                ("main".to_owned(), "tool_start:call_1:file_read".to_owned()),
                (
                    "main".to_owned(),
                    r#"tool_args:call_1:{"path":"a.txt"}"#.to_owned()
                ),
                ("main".to_owned(), "tool_end:call_1".to_owned()),
            ]
        );
    }

    #[test]
    fn an_output_item_that_is_not_a_function_call_produces_nothing() {
        // A finished message arrives on the same event type. Dispatching one
        // as a tool call would invent a call the model never made, and the
        // turn would then wait forever for a result nobody asked for.
        let got = frames_for(
            r#"{"type":"response.output_item.done","stream_id":"main",
                "item":{"type":"message","call_id":"call_1","name":"file_read"}}"#,
        );
        assert_eq!(summaries(&got), Vec::new());
    }

    #[test]
    fn a_completed_response_carries_its_id_and_usage() {
        // The one place the whole value is worth spelling out, because fold
        // A happens here: the response id becomes the run id, and the
        // provider's usage object becomes a one-entry AG-UI array naming the
        // model, which the provider never says.
        let got = frames_for(
            r#"{"type":"response.completed","stream_id":"main",
                "response":{"id":"resp_1",
                            "usage":{"input_tokens":10,"output_tokens":4,"total_tokens":14,
                                     "output_tokens_details":{"reasoning_tokens":3}}}}"#,
        );
        assert_eq!(
            got,
            vec![OutgoingFrame::Event {
                lane: "main".into(),
                event: Outgoing::RunFinished {
                    base: Base::default(),
                    thread_id: "main".into(),
                    run_id: "resp_1".into(),
                    usage: Some(vec![kobold_proto::agui::TokenUsage {
                        provider: Some("openai".into()),
                        model: Some("test-model".into()),
                        input_tokens: Some(10),
                        output_tokens: Some(4),
                        total_tokens: Some(14),
                        // Carried, where the old seam type had no field for
                        // it and dropped it in silence.
                        reasoning_tokens: Some(3),
                        cached_input_tokens: None,
                    }]),
                },
            }]
        );
    }

    #[test]
    fn a_turn_that_ends_without_usage_reports_none_rather_than_zero() {
        // Zero would read as "this turn was free" and the context gauge would
        // believe it.
        let got = frames_for(
            r#"{"type":"response.completed","stream_id":"main","response":{"id":"resp_1"}}"#,
        );
        assert_eq!(summaries(&got), one("main", "finished:resp_1:false"));
    }

    #[test]
    fn a_failure_is_reported_as_failed_and_never_also_as_completed() {
        // Both branches end a lane, so emitting both would complete a turn
        // that actually failed -- and the failure would be overwritten by the
        // success that followed it.
        let got = frames_for(
            r#"{"type":"response.failed","stream_id":"main",
                "error":{"code":"rate_limit","message":"slow down"}}"#,
        );
        assert_eq!(summaries(&got), one("main", "error:rate_limit:slow down"));
        assert!(
            !got.iter().any(|f| matches!(
                f,
                OutgoingFrame::Event {
                    event: Outgoing::RunFinished { .. },
                    ..
                }
            )),
            "a failed turn must not also finish"
        );
    }

    #[test]
    fn an_unknown_event_type_produces_nothing_at_all() {
        // The API grows event types; an unrecognised one must be inert rather
        // than an error, or every addition upstream breaks the client.
        assert_eq!(
            summaries(&frames_for(r#"{"type":"response.in_progress"}"#)),
            Vec::new()
        );
        assert_eq!(
            summaries(&frames_for(
                r#"{"type":"response.something.new","delta":"x"}"#
            )),
            Vec::new()
        );
    }

    use crate::ws::Incoming;
    use std::borrow::Cow;
    use std::collections::VecDeque;

    /// What a scripted provider does next.
    #[derive(Debug, Clone)]
    enum Act {
        /// One frame of wire JSON, exactly as the API would send it.
        Frame(String),
        /// The socket closes.
        Close,
        /// The server stops talking and never speaks again.
        Silence,
        /// Wait until the loop has actually sent a request before going on.
        ///
        /// `select!` picks at random among ready branches, so without this a
        /// script whose frames are instantly available can be consumed
        /// before the command is ever processed. Ordering by yielding rather
        /// than by sleeping: no timing, so it cannot be slow or flaky.
        AwaitSend,
    }

    /// A provider that says whatever the test tells it to, in whatever order.
    ///
    /// Deliberately able to misbehave. The happy path is the least valuable
    /// thing a fake can do -- the `Connected` ordering bug in the event loop
    /// was found by a fake that sent events in an order a real adapter would
    /// not, and every case below that matters is a shape the real API is not
    /// supposed to produce.
    struct FakeProvider {
        script: VecDeque<Act>,
        /// The frame most recently handed out. `Incoming` borrows from the
        /// transport, so it has to live somewhere that outlives the call.
        current: Vec<u8>,
        /// Everything the loop sent, so a test can assert on requests rather
        /// than only on what came back.
        pub sent: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl FakeProvider {
        fn new(script: Vec<Act>) -> Self {
            Self {
                script: script.into(),
                current: Vec::new(),
                sent: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            }
        }
    }

    impl ws::Transport for FakeProvider {
        type Error = String;

        async fn send_text(&mut self, text: &str) -> Result<(), String> {
            self.sent
                .lock()
                .expect("not poisoned")
                .push(text.to_owned());
            Ok(())
        }

        async fn read(&mut self) -> Result<Incoming<'_>, String> {
            while matches!(self.script.front(), Some(Act::AwaitSend)) {
                if self.sent.lock().expect("not poisoned").is_empty() {
                    tokio::task::yield_now().await;
                } else {
                    self.script.pop_front();
                }
            }
            match self.script.pop_front() {
                Some(Act::Frame(json)) => {
                    self.current = json.into_bytes();
                    Ok(Incoming::Text(Cow::Borrowed(&self.current)))
                }
                Some(Act::Close) => Ok(Incoming::Closed),
                // Never resolves, which is what a wedged server is.
                Some(Act::Silence) => std::future::pending().await,
                Some(Act::AwaitSend) => unreachable!("drained above"),
                // Running off the end of a script is a closed socket rather
                // than a hang, so a test that forgets one fails fast.
                None => Ok(Incoming::Closed),
            }
        }
    }

    fn frame(json: &str) -> Act {
        Act::Frame(json.to_owned())
    }

    /// Runs one turn against a scripted provider and collects every update.
    async fn turn_against(script: Vec<Act>) -> Vec<OutgoingFrame> {
        // Every script waits for the request first, so what the loop does
        // with the replies is never a race against how it sent them.
        let mut acts = vec![Act::AwaitSend];
        acts.extend(script);
        let provider = FakeProvider::new(acts);
        let (tx, mut urx) = mpsc::unbounded_channel();
        let (ctx, mut crx) = mpsc::unbounded_channel();
        let _ = ctx.send(Command::Send {
            lane: "main".to_owned(),
            text: "hello".to_owned(),
            previous_response_id: None,
            replay: Vec::new(),
        });
        // Held, not dropped. Dropping it closes the command channel, which
        // the loop correctly reads as "Kobold has gone, shut down" -- and it
        // would then return before reading a single frame. What ends these
        // runs is the transport, which is the thing under test.
        run_on(provider, &Model::default(), &tx, &mut crx).await;
        drop(ctx);
        let mut got = Vec::new();
        while let Ok(u) = urx.try_recv() {
            got.push(u);
        }
        got
    }

    /// Like `turn_against`, but keeps the reason the connection ended --
    /// which is the whole of what reconnect decides on.
    async fn ending_of(script: Vec<Act>) -> Ended {
        let mut acts = vec![Act::AwaitSend];
        acts.extend(script);
        let provider = FakeProvider::new(acts);
        let (tx, _urx) = mpsc::unbounded_channel();
        let (ctx, mut crx) = mpsc::unbounded_channel();
        let _ = ctx.send(Command::Send {
            lane: "main".to_owned(),
            text: "hello".to_owned(),
            previous_response_id: None,
            replay: Vec::new(),
        });
        let ended = run_on(provider, &Model::default(), &tx, &mut crx).await;
        drop(ctx);
        ended
    }

    /// A socket that dies mid-turn is retryable, and says so as a value
    /// rather than only as a message on screen.
    ///
    /// This is the milestone: before it, every one of these paths was a bare
    /// `return` and a session past the server's 60-minute cap simply ended.
    #[tokio::test]
    async fn a_socket_that_dies_mid_turn_asks_to_be_retried() {
        let ended = ending_of(vec![
            frame(r#"{"type":"response.output_text.delta","stream_id":"main","delta":"half a "}"#),
            Act::Close,
        ])
        .await;
        assert!(
            matches!(ended, Ended::Lost(_)),
            "a dropped socket must be retryable, got {ended:?}"
        );
    }

    /// The partner, and the one that stops "retry" meaning "retry always":
    /// Kobold saying stop must not reconnect.
    #[tokio::test]
    async fn a_quit_is_not_retried() {
        let provider = FakeProvider::new(vec![Act::Silence]);
        let (tx, _urx) = mpsc::unbounded_channel();
        let (ctx, mut crx) = mpsc::unbounded_channel();
        let _ = ctx.send(Command::Quit);
        let ended = run_on(provider, &Model::default(), &tx, &mut crx).await;
        assert_eq!(ended, Ended::Shutdown);
    }

    /// Kobold's own exit ends the adapter rather than reconnecting it. A
    /// socket kept open for a UI that has gone is a billable connection
    /// nobody is watching.
    #[tokio::test(start_paused = true)]
    async fn updates_that_reach_nobody_end_the_adapter() {
        let provider = FakeProvider::new(vec![
            Act::AwaitSend,
            frame(r#"{"type":"response.output_text.delta","stream_id":"main","delta":"hi"}"#),
            Act::Silence,
        ]);
        let (tx, urx) = mpsc::unbounded_channel();
        let (ctx, mut crx) = mpsc::unbounded_channel();
        let _ = ctx.send(Command::Send {
            lane: "main".to_owned(),
            text: "hello".to_owned(),
            previous_response_id: None,
            replay: Vec::new(),
        });
        drop(urx);
        // Bounded, because the thing being detected *is* a send -- so a
        // change that stops frames being produced stops this test noticing
        // anything, and the script's `Silence` then pends forever. That is a
        // hang rather than a failure, and cargo-mutants scores a hang as
        // indeterminate: `to_frames -> vec![]` was recorded as a TIMEOUT
        // here, not as a survivor and not as caught.
        //
        // Third time this file has needed the lesson, after `is_fatal` and
        // `backoff`: a test may not depend on the code under test in order to
        // finish. Virtual time, so the bound costs nothing.
        let ended = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            run_on(provider, &Model::default(), &tx, &mut crx),
        )
        .await
        .expect("the loop must notice a dead update channel rather than waiting on the socket");
        assert_eq!(
            ended,
            Ended::Shutdown,
            "a dead update channel must not reconnect"
        );
    }

    /// Rotation waits for the turn to finish.
    ///
    /// Driven by moving the clock rather than by waiting for 55 minutes: the
    /// deadline is paused-time, so this asserts the ordering exactly and
    /// takes no wall-clock at all. The turn is left deliberately *unfinished*
    /// -- a delta and then silence -- so the only thing that could end the
    /// run is the rotation branch, and it must not.
    #[tokio::test(start_paused = true)]
    async fn rotation_does_not_fire_while_a_turn_is_running() {
        let provider = FakeProvider::new(vec![
            Act::AwaitSend,
            frame(
                r#"{"type":"response.output_text.delta","stream_id":"main","delta":"still going"}"#,
            ),
            Act::Silence,
        ]);
        let (tx, _urx) = mpsc::unbounded_channel();
        let (ctx, mut crx) = mpsc::unbounded_channel();
        let _ = ctx.send(Command::Send {
            lane: "main".to_owned(),
            text: "hello".to_owned(),
            previous_response_id: None,
            replay: Vec::new(),
        });
        let model = Model::default();
        let run = run_on(provider, &model, &tx, &mut crx);
        tokio::pin!(run);
        // Well past the rotation deadline, with a response outstanding.
        let raced = tokio::time::timeout(ws::ROTATE_AFTER * 2, &mut run).await;
        assert!(
            raced.is_err(),
            "the loop rotated with a turn still in flight, which drops billed output: {raced:?}"
        );
        drop(ctx);
    }

    /// And the half that stops the test above being satisfied by a rotation
    /// that never fires at all: with nothing outstanding, it does.
    #[tokio::test(start_paused = true)]
    async fn rotation_fires_once_the_turn_has_ended() {
        let provider = FakeProvider::new(vec![
            Act::AwaitSend,
            frame(r#"{"type":"response.completed","stream_id":"main","response":{"id":"r1"}}"#),
            Act::Silence,
        ]);
        let (tx, _urx) = mpsc::unbounded_channel();
        let (ctx, mut crx) = mpsc::unbounded_channel();
        let _ = ctx.send(Command::Send {
            lane: "main".to_owned(),
            text: "hello".to_owned(),
            previous_response_id: None,
            replay: Vec::new(),
        });
        let ended = tokio::time::timeout(
            ws::ROTATE_AFTER * 2,
            run_on(provider, &Model::default(), &tx, &mut crx),
        )
        .await
        .expect("rotation must fire in an idle gap");
        assert_eq!(ended, Ended::Rotate);
        drop(ctx);
    }

    /// The reasoning summaries, mapped from a stream shaped like the one
    /// that was measured: a part opens, text streams token by token, the text
    /// closes, then the part closes.
    ///
    /// The `text.done` in the middle is in the script on purpose. It arrives
    /// before `part.done` and would also plausibly close the message, so
    /// emitting on both would end one reasoning message twice -- the
    /// assertion is that exactly one END comes out.
    #[tokio::test]
    async fn a_reasoning_summary_becomes_one_reasoning_message() {
        let got = turn_against(vec![
            frame(r#"{"type":"response.reasoning_summary_part.added","stream_id":"main","summary_index":0}"#),
            frame(r#"{"type":"response.reasoning_summary_text.delta","stream_id":"main","summary_index":0,"delta":"weighing "}"#),
            frame(r#"{"type":"response.reasoning_summary_text.delta","stream_id":"main","summary_index":0,"delta":"options"}"#),
            frame(r#"{"type":"response.reasoning_summary_text.done","stream_id":"main","summary_index":0}"#),
            frame(r#"{"type":"response.reasoning_summary_part.done","stream_id":"main","summary_index":0}"#),
            frame(r#"{"type":"response.completed","stream_id":"main","response":{"id":"r1"}}"#),
        ])
        .await;

        let ids: Vec<String> = got
            .iter()
            .filter_map(|f| match f {
                OutgoingFrame::Event {
                    event: Outgoing::ReasoningMessageStart { message_id, .. },
                    ..
                }
                | OutgoingFrame::Event {
                    event: Outgoing::ReasoningMessageEnd { message_id, .. },
                    ..
                } => Some(message_id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            ids,
            vec!["main:reasoning:0".to_owned(), "main:reasoning:0".to_owned()],
            "expected exactly one start and one end for the part: {got:?}"
        );

        let text: String = got
            .iter()
            .filter_map(|f| match f {
                OutgoingFrame::Event {
                    event: Outgoing::ReasoningMessageContent { delta, .. },
                    ..
                } => Some(delta.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            text, "weighing options",
            "the summary text was lost: {got:?}"
        );
    }

    /// Two parts in one turn are two messages, which is what `summary_index`
    /// is on the wire for.
    ///
    /// Without this, keying reasoning on the lane alone passes every
    /// assertion above and silently runs separate thoughts together.
    #[tokio::test]
    async fn separate_summary_parts_do_not_share_a_message() {
        let got = turn_against(vec![
            frame(r#"{"type":"response.reasoning_summary_part.added","stream_id":"main","summary_index":0}"#),
            frame(r#"{"type":"response.reasoning_summary_text.delta","stream_id":"main","summary_index":0,"delta":"first"}"#),
            frame(r#"{"type":"response.reasoning_summary_part.done","stream_id":"main","summary_index":0}"#),
            frame(r#"{"type":"response.reasoning_summary_part.added","stream_id":"main","summary_index":1}"#),
            frame(r#"{"type":"response.reasoning_summary_text.delta","stream_id":"main","summary_index":1,"delta":"second"}"#),
            frame(r#"{"type":"response.reasoning_summary_part.done","stream_id":"main","summary_index":1}"#),
            frame(r#"{"type":"response.completed","stream_id":"main","response":{"id":"r1"}}"#),
        ])
        .await;
        let mut ids: Vec<String> = got
            .iter()
            .filter_map(|f| match f {
                OutgoingFrame::Event {
                    event: Outgoing::ReasoningMessageContent { message_id, .. },
                    ..
                } => Some(message_id.clone()),
                _ => None,
            })
            .collect();
        ids.dedup();
        assert_eq!(
            ids,
            vec!["main:reasoning:0".to_owned(), "main:reasoning:1".to_owned()],
            "two summary parts collapsed into one message: {got:?}"
        );
    }

    /// A reasoning message must not be the same message as the answer, or
    /// anything keying on the id treats the summary and the reply as one
    /// stream of text.
    #[test]
    fn a_reasoning_message_is_not_the_lane() {
        assert_ne!(reasoning_id("main", Some(0)), "main");
        assert_ne!(reasoning_id("main", None), "main");
        // And distinct parts stay distinct, so the negative above is not
        // satisfied by a function returning one constant.
        assert_ne!(reasoning_id("main", Some(0)), reasoning_id("main", Some(1)));
        assert_ne!(reasoning_id("main", Some(0)), reasoning_id("side", Some(0)));
    }

    /// The request has to ask, or none of the above ever arrives. This is the
    /// whole reason a long think was 435 seconds of nothing.
    #[test]
    fn the_request_asks_for_reasoning_summaries() {
        let create = crate::events::ResponseCreate::user_text("m", Some("main"), "hi", "high");
        let body = json::to_string(&create).expect("serializes");
        assert!(
            body.contains(r#""summary":"auto""#),
            "no summary was requested: {body}"
        );
        assert!(
            body.contains(r#""effort":"high""#),
            "effort was dropped: {body}"
        );
    }

    /// A scripted sequence of connection attempts, so the reconnect loop can
    /// be driven without a network.
    ///
    /// Each entry is one call to `connect`: either a refusal with the reason
    /// as it would be shown, or a socket with a script of its own. Running
    /// out means "no more sockets, ever", which is how these tests end -- the
    /// loop would otherwise reconnect forever, which is the point of it.
    struct Dials {
        attempts: std::collections::VecDeque<Result<Vec<Act>, String>>,
        /// Every reason handed out, in order, so a test can assert how many
        /// times the loop actually tried.
        seen: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl Dials {
        fn new(attempts: Vec<Result<Vec<Act>, String>>) -> Self {
            Self {
                attempts: attempts.into(),
                seen: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            }
        }
    }

    impl Connect for Dials {
        type Conn = FakeProvider;
        async fn connect(&mut self) -> Result<FakeProvider, String> {
            match self.attempts.pop_front() {
                Some(Ok(script)) => {
                    self.seen
                        .lock()
                        .expect("not poisoned")
                        .push("connected".to_owned());
                    Ok(FakeProvider::new(script))
                }
                Some(Err(reason)) => {
                    self.seen.lock().expect("not poisoned").push(reason.clone());
                    Err(reason)
                }
                // The script is exhausted, and the reason is deliberately
                // **retryable**.
                //
                // It used to be a 401, so the loop would stop and the test
                // would end. That made every one of these tests depend on
                // `is_fatal` working in order to terminate at all -- so a
                // mutation breaking `is_fatal` did not fail them, it hung
                // them, and cargo-mutants recorded two TIMEOUTs. A timeout is
                // indeterminate: the suite did not detect the change, it
                // died. A real edit of that shape would look like
                // infrastructure flakiness rather than like a test naming a
                // defect.
                //
                // Retryable here means the loop only ever stops because
                // something under test decided it should, and `finished` is
                // the assertion. Callers bound the run in virtual time.
                //
                // Recorded like any other attempt: a fixture whose counter
                // skips one branch reports fewer tries than happened, which
                // is a debugging session starting from a false number. This
                // one did exactly that before it pushed here.
                None => {
                    let reason = "tcp: connection refused".to_owned();
                    self.seen.lock().expect("not poisoned").push(reason.clone());
                    Err(reason)
                }
            }
        }
    }

    /// Drive the reconnect loop over a scripted set of attempts.
    ///
    /// Returns whether the loop **finished on its own** alongside what it
    /// did. Finishing is a real outcome and not a test-harness detail: the
    /// loop is supposed to run forever until something tells it to stop, so
    /// "did it stop" is the assertion for every giving-up rule, and "did it
    /// keep going" is the assertion for every retrying one.
    ///
    /// Bounded in virtual time, which costs no wall-clock -- every wait in
    /// the loop is a `tokio::time::sleep`, so a paused clock skips them and
    /// the budget below is reached in microseconds.
    async fn reconnects_over(
        attempts: Vec<Result<Vec<Act>, String>>,
    ) -> (bool, Vec<Transport>, Vec<String>) {
        let dials = Dials::new(attempts);
        let seen = std::sync::Arc::clone(&dials.seen);
        let (tx, mut urx) = mpsc::unbounded_channel();
        let (ctx, crx) = mpsc::unbounded_channel();
        let _ = ctx.send(Command::Send {
            lane: "main".to_owned(),
            text: "hello".to_owned(),
            previous_response_id: None,
            replay: Vec::new(),
        });
        // **Bounded by iterations, and never by a duration.** The obvious
        // spelling is `tokio::time::timeout` around the loop, and it was
        // written that way first. It hangs: a paused clock only auto-advances
        // when the runtime is idle, so a `backoff` of zero spins the loop
        // forever and the timeout that was supposed to bound it never fires.
        // cargo-mutants reported that as two TIMEOUTs on `backoff` -- and a
        // timeout is indeterminate, not caught. The suite did not detect the
        // change, it died, which reads as flakiness rather than as a test
        // naming a defect.
        //
        // Driving the clock by hand instead means no value any production
        // function returns can stop this terminating. 200 steps of a minute
        // is past the 55-minute rotation deadline and past any backoff.
        let handle = tokio::spawn(run_with(dials, Model::default(), tx, crx));
        for _ in 0..200 {
            if handle.is_finished() {
                break;
            }
            tokio::task::yield_now().await;
            tokio::time::advance(std::time::Duration::from_secs(60)).await;
        }
        let finished = handle.is_finished();
        handle.abort();
        drop(ctx);
        let mut transports = Vec::new();
        while let Ok(u) = urx.try_recv() {
            if let OutgoingFrame::Transport(t) = u {
                transports.push(t);
            }
        }
        let seen = seen.lock().expect("not poisoned").clone();
        (finished, transports, seen)
    }

    /// The milestone, end to end: a socket dies and the loop opens another.
    ///
    /// Before this, `net::run` connected once and every failure path was a
    /// bare `return`. A mutation run is what said the loop was untested --
    /// replacing `run` with an empty body survived the whole suite, because
    /// connecting was hard-coded and nothing could reach it.
    #[tokio::test(start_paused = true)]
    async fn a_dropped_socket_is_followed_by_another_one() {
        let (finished, transports, seen) = reconnects_over(vec![
            Ok(vec![Act::AwaitSend, Act::Close]),
            // No `AwaitSend` on the second socket, and this is a property of
            // the fixture rather than a detail: `AwaitSend` spins until a
            // command is sent, only one ever is, so waiting for a second
            // would hang rather than fail.
            Ok(vec![Act::Close]),
        ])
        .await;
        let connects = transports
            .iter()
            .filter(|t| matches!(t, Transport::Connected))
            .count();
        assert_eq!(connects, 2, "the loop did not reconnect: {transports:?}");
        assert!(
            seen.len() > 2,
            "the loop stopped trying after its script: {seen:?}"
        );
        assert!(
            !finished,
            "a retryable failure ended the session: {transports:?}"
        );
    }

    /// And the partner, which is the one that matters for the user's bill: a
    /// rejected key must not be retried at all.
    ///
    /// Without this, "reconnects on failure" is satisfied by a loop that
    /// hammers the provider with a bad credential until someone notices.
    #[tokio::test(start_paused = true)]
    async fn a_rejected_credential_is_not_retried() {
        let (finished, transports, seen) = reconnects_over(vec![
            Err("websocket: Invalid status code: 401".to_owned()),
            // Never reached. Present so the assertion below is about the
            // loop stopping, not about the script running out.
            Ok(vec![Act::Close]),
        ])
        .await;
        // **`finished` is the load-bearing one.** Everything else here is
        // also true of a loop that simply had not got round to retrying yet;
        // only stopping distinguishes "gave up" from "still going".
        assert!(finished, "a rejected credential was retried: {seen:?}");
        assert_eq!(seen.len(), 1, "a 401 was retried: {seen:?}");
        assert!(
            transports
                .iter()
                .all(|t| !matches!(t, Transport::Connected)),
            "nothing should have connected: {transports:?}"
        );
    }

    /// A transient refusal is retried, which is the other half of the
    /// classification and the reason the default is to retry.
    #[tokio::test(start_paused = true)]
    async fn a_refused_connection_is_tried_again() {
        let (finished, transports, seen) = reconnects_over(vec![
            Err("tcp: connection refused".to_owned()),
            Err("tcp: connection refused".to_owned()),
            Ok(vec![Act::Close]),
        ])
        .await;
        assert!(
            !finished,
            "the loop gave up on a retryable failure: {seen:?}"
        );
        assert!(
            seen.len() >= 3,
            "the loop gave up on a retryable failure: {seen:?}"
        );
        assert_eq!(
            transports
                .iter()
                .filter(|t| matches!(t, Transport::Connected))
                .count(),
            1,
            "the third attempt should have connected: {transports:?}"
        );
    }

    /// Rotation is silent. The user is told when something is wrong, and a
    /// connection reaching its age limit in an idle gap is not that -- a
    /// `Disconnected` here would put a red marker on every pane for a
    /// housekeeping step they cannot act on.
    #[tokio::test(start_paused = true)]
    async fn rotating_does_not_look_like_a_failure() {
        let (_finished, transports, _) = reconnects_over(vec![
            // Completes the turn, then goes quiet: the rotation deadline is
            // what ends this socket, in an idle gap.
            Ok(vec![
                Act::AwaitSend,
                frame(r#"{"type":"response.completed","stream_id":"main","response":{"id":"r1"}}"#),
                Act::Silence,
            ]),
            Ok(vec![Act::Close]),
        ])
        .await;
        let before_second_connect: Vec<_> = transports
            .iter()
            .take_while(|t| !matches!(t, Transport::Connected))
            .chain(
                transports
                    .iter()
                    .skip_while(|t| !matches!(t, Transport::Connected))
                    .skip(1)
                    .take_while(|t| !matches!(t, Transport::Connected)),
            )
            .collect();
        assert!(
            before_second_connect
                .iter()
                .all(|t| !matches!(t, Transport::Disconnected(_))),
            "rotation was announced as a disconnection: {transports:?}"
        );
        assert_eq!(
            transports
                .iter()
                .filter(|t| matches!(t, Transport::Connected))
                .count(),
            2,
            "rotation should have opened a second socket: {transports:?}"
        );
    }

    /// A rejected credential is answered by editing configuration, never by
    /// waiting, so it must not be retried. Both directions, because
    /// "everything is fatal" and "nothing is fatal" each satisfy one half.
    #[test]
    fn only_an_answer_a_new_socket_would_repeat_is_fatal() {
        assert!(is_fatal("websocket: Invalid status code: 401"));
        assert!(is_fatal("websocket: Invalid status code: 403"));
        assert!(!is_fatal("tcp: connection reset by peer"));
        assert!(!is_fatal("tls: handshake eof"));
        assert!(!is_fatal("server closed the connection"));
        // A 500 is the provider having a bad minute, which is the case retry
        // exists for.
        assert!(!is_fatal("websocket: Invalid status code: 500"));
    }

    /// The ceiling is the point of the backoff, so it is what gets asserted:
    /// an hour-long outage must be neither a tight loop nor an exponentially
    /// receding one.
    #[test]
    fn backoff_grows_and_then_stops_growing() {
        assert_eq!(backoff(0), std::time::Duration::from_secs(1));
        assert!(backoff(3) > backoff(1), "it must actually back off");
        let capped = backoff(50);
        assert_eq!(capped, std::time::Duration::from_secs(30));
        assert_eq!(
            backoff(u32::MAX),
            capped,
            "no attempt count may overflow the shift"
        );
    }

    #[tokio::test]
    async fn a_normal_turn_streams_and_completes() {
        let got = turn_against(vec![
            frame(r#"{"type":"response.output_text.delta","stream_id":"main","delta":"hi "}"#),
            frame(r#"{"type":"response.output_text.delta","stream_id":"main","delta":"there"}"#),
            frame(r#"{"type":"response.completed","stream_id":"main","response":{"id":"r1"}}"#),
        ])
        .await;
        assert_eq!(
            summaries(&got),
            vec![
                ("main".to_owned(), "delta:hi ".to_owned()),
                ("main".to_owned(), "delta:there".to_owned()),
                ("main".to_owned(), "finished:r1:false".to_owned()),
                // The script running out is the socket closing, and the loop
                // reports that rather than returning quietly.
                (
                    String::new(),
                    "disconnected:server closed the connection".to_owned()
                ),
            ]
        );
    }

    #[tokio::test]
    async fn a_tool_call_reaches_the_ui_even_with_no_arguments() {
        // A function call whose arguments field is absent. The API should
        // always send one, which is exactly why it is worth checking: an
        // unwrap here would take the whole session down on a malformed
        // frame, and an empty string is something the tool layer can refuse
        // and report.
        let got = turn_against(vec![frame(
            r#"{"type":"response.output_item.done","stream_id":"main",
                "item":{"type":"function_call","call_id":"c1","name":"file_read"}}"#,
        )])
        .await;
        // The arguments frame specifically: an absent `arguments` must reach
        // Kobold as an empty string it can refuse and report, not as a
        // missing frame that leaves the call half-open.
        assert_eq!(
            got.iter()
                .map(summary)
                .find(|(_, what)| what.starts_with("tool_args")),
            Some(("main".to_owned(), "tool_args:c1:".to_owned()))
        );
        // And the boundary is still complete around it.
        assert_eq!(
            summaries(&got)[..3],
            [
                ("main".to_owned(), "tool_start:c1:file_read".to_owned()),
                ("main".to_owned(), "tool_args:c1:".to_owned()),
                ("main".to_owned(), "tool_end:c1".to_owned()),
            ]
        );
    }

    #[tokio::test]
    async fn a_tool_call_ends_its_run_and_the_boundary_falls_in_the_order_the_spec_requires() {
        // **The run boundary already exists on this provider's wire, and that
        // is the finding rather than a change.** `Command::ToolResult` opens
        // a *new* response with `previous_response_id` pointing at the old
        // one, so one Kobold turn was already several provider responses --
        // and a response boundary is exactly where AG-UI puts a run boundary.
        //
        // What has to hold is the order: `TOOL_CALL_END` before
        // `RUN_FINISHED`, never the reverse. Reversed, Kobold settles the
        // request before the call it has to answer has arrived, and the pane
        // goes idle with a tool call outstanding.
        let got = turn_against(vec![
            frame(
                r#"{"type":"response.output_item.done","stream_id":"main",
                    "item":{"type":"function_call","call_id":"c1","name":"file_read",
                            "arguments":"{}"}}"#,
            ),
            frame(r#"{"type":"response.completed","stream_id":"main","response":{"id":"r1"}}"#),
        ])
        .await;
        let what: Vec<String> = summaries(&got).into_iter().map(|(_, w)| w).collect();
        let end = what
            .iter()
            .position(|w| w == "tool_end:c1")
            .expect("no tool_end");
        let finished = what
            .iter()
            .position(|w| w.starts_with("finished:"))
            .expect("the run never ended");
        assert!(
            end < finished,
            "the run ended before the call it has to answer: {what:?}"
        );

        // Exactly one run end for one response. A second would settle a
        // request that was never made and drop the count below what is
        // actually outstanding.
        assert_eq!(
            what.iter().filter(|w| w.starts_with("finished:")).count(),
            1,
            "{what:?}"
        );
    }

    #[tokio::test]
    async fn a_failure_mid_stream_ends_the_lane_and_keeps_what_came_before() {
        let got = turn_against(vec![
            frame(r#"{"type":"response.output_text.delta","stream_id":"main","delta":"half"}"#),
            frame(
                r#"{"type":"response.failed","stream_id":"main",
                    "error":{"code":"overloaded","message":"try later"}}"#,
            ),
        ])
        .await;
        assert!(got.iter().any(|f| summary(f).1 == "delta:half"));
        assert!(got
            .iter()
            .any(|f| summary(f).1.starts_with("error:overloaded:")));
        assert!(
            !got.iter().any(|f| summary(f).1.starts_with("finished:")),
            "a failed turn must not also complete"
        );
    }

    #[tokio::test]
    async fn a_socket_that_closes_without_a_terminal_event_is_reported() {
        // The shape that leaves a turn hanging forever if it is missed: the
        // lane never completed and never failed, so nothing downstream knows
        // the turn is over unless the close itself says so.
        let got = turn_against(vec![
            frame(r#"{"type":"response.output_text.delta","stream_id":"main","delta":"half"}"#),
            Act::Close,
        ])
        .await;
        assert!(
            matches!(
                got.last(),
                Some(OutgoingFrame::Transport(Transport::Disconnected(_)))
            ),
            "a close mid-turn must be reported: {got:?}"
        );
    }

    #[tokio::test]
    async fn a_delta_after_completion_is_still_delivered_rather_than_dropped() {
        // Out of order on purpose. The API is not supposed to do this, and
        // the loop has no state saying a lane is finished -- so the honest
        // behaviour is to pass it on and let the UI decide. Pinned because
        // the tempting "optimisation" is to filter it, which would silently
        // discard real text the moment the assumption was wrong.
        let got = turn_against(vec![
            frame(r#"{"type":"response.completed","stream_id":"main","response":{"id":"r1"}}"#),
            frame(r#"{"type":"response.output_text.delta","stream_id":"main","delta":"after"}"#),
        ])
        .await;
        assert!(
            got.iter().any(|f| summary(f).1 == "delta:after"),
            "a late delta was dropped: {got:?}"
        );
    }

    #[tokio::test]
    async fn a_terminal_event_twice_produces_two_completions_rather_than_a_panic() {
        // Also not something the API should send. What matters is that it
        // does not corrupt anything: the loop keeps no per-lane completion
        // state, so a repeat is simply reported twice.
        let done = r#"{"type":"response.completed","stream_id":"main","response":{"id":"r1"}}"#;
        let got = turn_against(vec![frame(done), frame(done)]).await;
        assert_eq!(
            got.iter()
                .filter(|f| summary(f).1.starts_with("finished:"))
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn an_unknown_event_type_is_ignored_and_the_turn_carries_on() {
        // The API grows event types. An unrecognised one must be inert, or
        // every addition upstream breaks the client.
        let got = turn_against(vec![
            frame(r#"{"type":"response.something.new","stream_id":"main"}"#),
            frame(
                r#"{"type":"response.output_text.delta","stream_id":"main","delta":"still here"}"#,
            ),
            frame(r#"{"type":"response.completed","stream_id":"main","response":{"id":"r1"}}"#),
        ])
        .await;
        assert!(got.iter().any(|f| summary(f).1 == "delta:still here"));
        assert!(got.iter().any(|f| summary(f).1.starts_with("finished:")));
    }

    #[tokio::test]
    async fn a_provider_that_goes_silent_leaves_the_adapter_waiting_rather_than_inventing_an_end() {
        // The wedge, from the adapter's side. It has no timeout of its own
        // and must not grow one: Kobold owns that policy, and it has two
        // thresholds derived from measurement -- 435 seconds of legitimate
        // silence before a first delta against a 31ms worst case mid-stream.
        // An adapter that gave up on its own schedule would preempt that and
        // kill reasoning turns Kobold would have let run.
        //
        // So what is asserted is that nothing happens: the deltas before the
        // silence arrive, no terminal update is invented, and the loop is
        // still running when the test gives up on it.
        let provider = FakeProvider::new(vec![
            Act::AwaitSend,
            frame(r#"{"type":"response.output_text.delta","stream_id":"main","delta":"half"}"#),
            Act::Silence,
        ]);
        let (tx, mut urx) = mpsc::unbounded_channel();
        let (ctx, mut crx) = mpsc::unbounded_channel();
        let _ = ctx.send(Command::Send {
            lane: "main".to_owned(),
            text: "hello".to_owned(),
            previous_response_id: None,
            replay: Vec::new(),
        });

        let ended = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            run_on(provider, &Model::default(), &tx, &mut crx),
        )
        .await;
        assert!(
            ended.is_err(),
            "the adapter ended a turn the provider never ended"
        );
        drop(ctx);

        let mut got = Vec::new();
        while let Ok(u) = urx.try_recv() {
            got.push(u);
        }
        assert_eq!(
            summaries(&got),
            vec![("main".to_owned(), "delta:half".to_owned())],
            "the adapter invented an update the provider never sent"
        );
    }

    #[tokio::test]
    async fn the_request_the_loop_sends_carries_the_lane_and_the_prompt() {
        // The counter-assertion to every test above: they all script the
        // replies, so they would pass even if the loop sent nothing at all.
        // This reads what actually went out.
        let provider = FakeProvider::new(vec![Act::AwaitSend, Act::Close]);
        let sent = provider.sent.clone();
        let (tx, _urx) = mpsc::unbounded_channel();
        let (ctx, mut crx) = mpsc::unbounded_channel();
        let _ = ctx.send(Command::Send {
            lane: "fork-1".to_owned(),
            text: "the question".to_owned(),
            previous_response_id: Some("r0".to_owned()),
            replay: Vec::new(),
        });
        run_on(provider, &Model::default(), &tx, &mut crx).await;
        drop(ctx);

        let sent = sent.lock().expect("not poisoned");
        assert_eq!(sent.len(), 1, "expected exactly one request, got {sent:?}");
        let body = &sent[0];
        assert!(
            body.contains("\"stream_id\":\"fork-1\""),
            "lane missing from {body}"
        );
        assert!(body.contains("the question"), "prompt missing from {body}");
        assert!(
            body.contains("\"previous_response_id\":\"r0\""),
            "chain missing from {body}"
        );
    }
}
