//! TUI state and rendering.

use ratatui::buffer::{Buffer, CellWidth};
use ratatui::layout::{Constraint, Layout, Margin, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;

use crate::net::{Incoming, Transport};

/// What the keyboard is driving. Shown in the bottom bar so the current
/// binding set is never a guess.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Mode {
    /// Typing at the prompt. Up/Down walk input history.
    Send,
    /// Browsing the transcript. Shift+Up/Down move the selection.
    Chat,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Send => "send",
            Mode::Chat => "chat",
        }
    }
}

/// A tool call being assembled from the events that describe it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingCall {
    pub id: String,
    pub name: String,
    /// Concatenated from every `TOOL_CALL_ARGS` delta. One event carries the
    /// whole thing today; the protocol permits any number.
    pub arguments: String,
}

/// What an event asks the caller to do, when applying it is not the whole of
/// it.
///
/// Returned rather than done here because `App` owns no IO and cannot run a
/// tool -- the same reason the tool seam has always been in `main.rs`. It is
/// a return value rather than a callback so the decision stays testable
/// without a runtime.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    None,
    /// A call the model finished describing. Whoever gets this runs it and
    /// sends the result back.
    RunTool(crate::tools::Call),
}

/// What a pane is doing, derived rather than stored.
///
/// Carries no payload: the reason a connection went away lives on `Link`,
/// where it is set once, and `Pane::gone_reason` reads it. That keeps this
/// `Copy` and free to compute, which matters because the render path asks
/// every pane for it on every frame.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Status {
    Connecting,
    Ready,
    Waiting,
    Gone,
}

/// The transport's state, which is about the connection and not about any
/// turn.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Link {
    Connecting,
    Up,
    Gone(String),
}

/// One field of a bottom form panel -- the shape the `ask` local tool and an
/// MCP elicitation request both render into, so the two share one widget
/// rather than each growing its own.
#[derive(Debug, Clone, PartialEq)]
pub enum Field {
    /// Up to `tools::MAX_OPTIONS` choices, radio (`multi: false`) or
    /// checkbox (`multi: true`).
    Choice {
        prompt: String,
        options: Vec<String>,
        multi: bool,
    },
    /// Free text, e.g. `ask`'s always-available fifth option.
    Text { prompt: String },
}

/// What became of a panel. A cancel is kept apart from an empty submission --
/// `ask`'s caller reports the two differently to the model, and a future
/// elicitation caller needs the same distinction for MCP's decline/cancel.
#[derive(Debug, Clone, PartialEq)]
pub enum PanelOutcome {
    /// One answer per field, in field order. A `Choice` field's answer is its
    /// chosen options joined by `", "` (empty if none were chosen).
    Submitted(Vec<String>),
    Cancelled,
}

impl PanelOutcome {
    /// The one answer that counts, for a panel like `ask`'s where the fields
    /// are alternatives rather than a form -- whichever field came back
    /// non-empty, first field first, since a `Choice` field precedes its
    /// trailing free-text fallback in `From<&Ask>`.
    pub fn primary_answer(&self) -> Option<&str> {
        match self {
            PanelOutcome::Submitted(answers) => {
                answers.iter().find(|a| !a.is_empty()).map(String::as_str)
            }
            PanelOutcome::Cancelled => None,
        }
    }
}

/// Per-field answer state. Mirrors `Panel::fields` one to one.
#[derive(Debug, Clone)]
enum FieldState {
    /// One flag per option; more than one may be set even for a radio field
    /// mid-interaction, since `Panel::select` is what enforces "only one" --
    /// this just stores whatever it was told.
    Choice(Vec<bool>),
    /// The caret is kept with the text so the field takes the same keys as the
    /// prompt. Without one it could only append and delete from the end, which
    /// is why a question asking for a sentence could not be given one.
    Text { text: String, cursor: usize },
}

/// A bottom form panel: a fixed sequence of fields, one flat list of rows
/// (each `Choice` option is a row, each `Text` field is one row) that the
/// up/down keys walk without needing to know which field a row belongs to.
pub struct Panel {
    pub fields: Vec<Field>,
    state: Vec<FieldState>,
    /// Highlighted row, indexing into `Panel::rows()`.
    row: usize,
    /// Which call each field answers, one per field.
    ///
    /// A panel can carry questions from several calls at once: the model may
    /// ask two things in one turn, and the API allows it -- `parallel_tool_calls`
    /// is on. Showing them one after another made a conversation of what was
    /// meant to be a single form, so they share a panel and each call gets its
    /// own result back.
    owners: Vec<String>,
    /// Opaque correlation id of the first call, kept because the panel is
    /// matched by token when it resolves. The panel never inspects it.
    pub token: String,
}

impl Panel {
    pub fn new(token: String, fields: Vec<Field>) -> Self {
        let state = fields
            .iter()
            .map(|f| match f {
                Field::Choice { options, .. } => FieldState::Choice(vec![false; options.len()]),
                Field::Text { .. } => FieldState::Text {
                    text: String::new(),
                    cursor: 0,
                },
            })
            .collect();
        let owners = vec![token.clone(); fields.len()];
        let mut panel = Self {
            fields,
            state,
            row: 0,
            owners,
            token,
        };
        // Landing on a radio option selects it (see `select`), so the row the
        // panel opens on should already read as chosen rather than the first
        // keypress being needed just to establish that.
        panel.select(true);
        panel
    }

    /// `(field index, option index)` for each row, in display order. A `Text`
    /// field contributes one row with `option index = None`.
    fn rows(&self) -> Vec<(usize, Option<usize>)> {
        self.fields
            .iter()
            .enumerate()
            .flat_map(|(fi, f)| -> Vec<(usize, Option<usize>)> {
                match f {
                    Field::Choice { options, .. } => {
                        (0..options.len()).map(|oi| (fi, Some(oi))).collect()
                    }
                    Field::Text { .. } => vec![(fi, None)],
                }
            })
            .collect()
    }

    pub fn row_count(&self) -> usize {
        self.rows().len()
    }

    /// The row the cursor is on, as `(field index, option index)`.
    fn current(&self) -> (usize, Option<usize>) {
        self.rows()[self.row]
    }

    /// Move the highlight, wrapping at both ends -- same convention as the
    /// slash-command menu's `menu_move`. A radio field auto-selects the row
    /// the highlight lands on.
    pub fn move_row(&mut self, delta: isize) -> bool {
        let n = self.row_count();
        if n == 0 {
            return false;
        }
        let before = self.owners[self.current().0].clone();
        self.row = (self.row as isize + delta).rem_euclid(n as isize) as usize;
        let after = self.owners[self.current().0].clone();
        // Moving inside one question moves its radio selection, which is what
        // lets a single-question panel be answered with the arrows alone. The
        // unit is the question, not the field: its options and the free-text
        // row beneath them are separate fields and the same question, so
        // stepping between them still counts as staying put.
        //
        // Crossing into another question only selects if that one has no
        // answer yet -- so it reads as chosen the moment it is reached, but
        // walking back and forth cannot rewrite an answer already given.
        self.select(before == after);
        true
    }

    /// Landing on a radio option selects it; landing anywhere else is a
    /// no-op. Called after every row move so highlight and selection can
    /// never disagree for a radio field, matching how the slash-command menu
    /// treats "highlighted" as "chosen".
    fn select(&mut self, force: bool) {
        let (fi, oi) = self.current();
        if let (Field::Choice { multi: false, .. }, FieldState::Choice(chosen)) =
            (&self.fields[fi], &mut self.state[fi])
        {
            let Some(oi) = oi else { return };
            if !force && chosen.iter().any(|c| *c) {
                return;
            }
            chosen
                .iter_mut()
                .enumerate()
                .for_each(|(i, c)| *c = i == oi);
        }
    }

    /// Space on a radio row: move the selection here.
    ///
    /// Needed now that landing no longer selects: without it a radio answer
    /// could be established but never changed.
    fn pick(&mut self) -> bool {
        let (fi, oi) = self.current();
        if let (Field::Choice { multi: false, .. }, FieldState::Choice(chosen), Some(oi)) =
            (&self.fields[fi], &mut self.state[fi], oi)
        {
            if chosen.get(oi) == Some(&true) {
                return false;
            }
            chosen
                .iter_mut()
                .enumerate()
                .for_each(|(i, c)| *c = i == oi);
            return true;
        }
        false
    }

    /// Space: toggle membership in a checkbox row. No effect on a radio row
    /// (highlighting already selects it) or a text row (there is nothing to
    /// toggle).
    pub fn toggle(&mut self) -> bool {
        if self.pick() {
            return true;
        }
        let (fi, oi) = self.current();
        if let (Field::Choice { multi: true, .. }, FieldState::Choice(chosen), Some(oi)) =
            (&self.fields[fi], &mut self.state[fi], oi)
        {
            chosen[oi] = !chosen[oi];
            return true;
        }
        false
    }

    /// The row index for a given `(field, option)`, for jumping the
    /// highlight straight there rather than walking it with `move_row`.
    fn row_of(&self, fi: usize, oi: Option<usize>) -> Option<usize> {
        self.rows().iter().position(|&r| r == (fi, oi))
    }

    /// A digit typed while the highlight is on a choice row: `1` picks that
    /// question's first option, and so on. `false` when the highlight is not
    /// on a choice row, or the digit names no option of it -- both cases the
    /// caller falls through to `jump_to_own_text`, so a stray or out-of-range
    /// digit still lands somewhere instead of being silently dropped.
    ///
    /// Scoped to the field the highlight already sits on, never to the
    /// panel as a whole: `tools::MAX_OPTIONS` keeps each question's own
    /// numbering a single digit, and a panel-wide count would make the same
    /// key mean a different option depending on how many earlier questions
    /// came before it.
    fn select_option(&mut self, digit: u32) -> bool {
        let (fi, oi) = self.current();
        if oi.is_none() {
            return false;
        }
        let Field::Choice { options, .. } = &self.fields[fi] else {
            return false;
        };
        let target = match (digit as usize).checked_sub(1) {
            Some(i) if i < options.len() => i,
            _ => return false,
        };
        let Some(row) = self.row_of(fi, Some(target)) else {
            return false;
        };
        self.row = row;
        // A digit is an explicit act, unlike landing on a row while walking
        // with the arrows -- so unlike `move_row`'s restraint against
        // rewriting an answer crossed over incidentally, this is allowed to
        // overwrite an answer this question already had. It can never reach
        // another question's state regardless: `fi` above is always the
        // field the highlight was already on, never one `select_option`
        // chooses to visit.
        //
        // `toggle`'s own return is ignored here on purpose: it reports
        // false for "already exactly this, nothing to repaint", which is a
        // real answer to "did the screen change" but the wrong answer to
        // "did this digit mean something" -- and the caller uses this
        // return for the latter. Getting that wrong would send a repeat of
        // an already-matched digit past `char`'s dispatch and into
        // `jump_to_own_text`, typing it as a literal character instead of
        // leaving an already-correct selection alone.
        self.toggle();
        true
    }

    /// Not a matching digit, with the highlight on a choice row: the
    /// character is answered as free text rather than dropped, on the
    /// question the highlight is already on -- so a letter typed while
    /// answering question 2 lands under question 2, not question 1.
    fn jump_to_own_text(&mut self, c: char) -> bool {
        let (fi, oi) = self.current();
        if oi.is_none() {
            return false;
        }
        let owner = self.owners[fi].clone();
        let Some(ti) = self
            .owners
            .iter()
            .enumerate()
            .position(|(i, o)| *o == owner && matches!(self.fields[i], Field::Text { .. }))
        else {
            return false;
        };
        let Some(row) = self.row_of(ti, None) else {
            return false;
        };
        self.row = row;
        self.type_char(c)
    }

    /// A character typed while the panel is up and the key was not one of
    /// its motions or controls. On a text row this is ordinary typing --
    /// digits included, or a question asking "how many retries?" could
    /// never be answered "3". On a choice row a digit that names one of the
    /// question's own options selects it (or, for a checkbox, toggles it);
    /// anything else, including a digit with no matching option, jumps to
    /// that question's free-text field and types there -- typing an answer
    /// no longer needs arrowing down to the text row first.
    pub fn char(&mut self, c: char) -> bool {
        // Checked against the field the highlight is on right now, not the
        // panel as a whole -- a digit must stay literal once the highlight
        // has already reached free text, or an answer like "3" could never
        // be typed. Do not "simplify" this to a panel-level check.
        if self.text_mut().is_some() {
            return self.type_char(c);
        }
        if let Some(d) = c.to_digit(10) {
            if self.select_option(d) {
                return true;
            }
        }
        self.jump_to_own_text(c)
    }

    /// Ordinary typing, only live on the row it lands on.
    /// The text and caret of the field the highlight is on, when it is a text
    /// field. `None` on a choice row, where typing means something else.
    fn text_mut(&mut self) -> Option<(&mut String, &mut usize)> {
        let (fi, oi) = self.current();
        if oi.is_some() {
            return None;
        }
        match &mut self.state[fi] {
            FieldState::Text { text, cursor } => Some((text, cursor)),
            FieldState::Choice(_) => None,
        }
    }

    /// The text of the focused field, when it is a text one.
    pub fn focused_text(&self) -> Option<String> {
        let (fi, oi) = self.current();
        if oi.is_some() {
            return None;
        }
        match &self.state[fi] {
            FieldState::Text { text, .. } => Some(text.clone()),
            FieldState::Choice(_) => None,
        }
    }

    /// Where the caret sits in the focused text field, in characters, for the
    /// renderer to place a terminal cursor.
    pub fn text_caret(&self) -> Option<usize> {
        let (fi, oi) = self.current();
        if oi.is_some() {
            return None;
        }
        match &self.state[fi] {
            FieldState::Text { cursor, .. } => Some(*cursor),
            FieldState::Choice(_) => None,
        }
    }

    // The prompt's keys, on the panel's text field. Each is the same operation
    // the message input performs, because they are literally the same code --
    // see `crate::edit`.
    pub fn type_char(&mut self, c: char) -> bool {
        self.text_mut()
            .is_some_and(|(t, k)| crate::edit::insert(t, k, c))
    }

    pub fn paste(&mut self, run: &str) -> bool {
        self.text_mut()
            .is_some_and(|(t, k)| crate::edit::insert_str(t, k, run))
    }

    pub fn backspace(&mut self) -> bool {
        self.text_mut()
            .is_some_and(|(t, k)| crate::edit::backspace(t, k))
    }

    pub fn delete_forward(&mut self) -> bool {
        self.text_mut()
            .is_some_and(|(t, k)| crate::edit::delete_forward(t, k))
    }

    pub fn move_left(&mut self, word: bool) -> bool {
        self.text_mut()
            .is_some_and(|(t, k)| crate::edit::move_left(t, k, word))
    }

    pub fn move_right(&mut self, word: bool) -> bool {
        self.text_mut()
            .is_some_and(|(t, k)| crate::edit::move_right(t, k, word))
    }

    pub fn home(&mut self) -> bool {
        self.text_mut().is_some_and(|(_, k)| crate::edit::home(k))
    }

    pub fn end(&mut self) -> bool {
        self.text_mut().is_some_and(|(t, k)| crate::edit::end(t, k))
    }

    pub fn delete_word_back(&mut self) -> bool {
        self.text_mut()
            .is_some_and(|(t, k)| crate::edit::delete_word_back(t, k))
    }

    pub fn kill_to_start(&mut self) -> bool {
        self.text_mut()
            .is_some_and(|(t, k)| crate::edit::kill_to_start(t, k))
    }

    pub fn kill_to_end(&mut self) -> bool {
        self.text_mut()
            .is_some_and(|(t, k)| crate::edit::kill_to_end(t, k))
    }

    /// The text currently typed on the given field's row, for rendering a
    /// `Text` row's live value.
    fn text_of(&self, field: usize) -> &str {
        match &self.state[field] {
            FieldState::Text { text: v, .. } => v,
            FieldState::Choice(_) => "",
        }
    }

    /// Whether the given option of a `Choice` field is currently chosen, for
    /// rendering its marker.
    fn chosen(&self, field: usize, option: usize) -> bool {
        match &self.state[field] {
            FieldState::Choice(c) => c[option],
            FieldState::Text { .. } => false,
        }
    }

    pub fn submit(&self) -> PanelOutcome {
        let answers = self
            .fields
            .iter()
            .zip(&self.state)
            .map(|(f, s)| match (f, s) {
                (Field::Choice { options, .. }, FieldState::Choice(chosen)) => options
                    .iter()
                    .zip(chosen)
                    .filter(|(_, c)| **c)
                    .map(|(o, _)| o.clone())
                    .collect::<Vec<_>>()
                    .join(", "),
                (Field::Text { .. }, FieldState::Text { text, .. }) => text.clone(),
                _ => unreachable!("Panel::state mirrors Panel::fields one to one"),
            })
            .collect();
        PanelOutcome::Submitted(answers)
    }
}

impl From<&crate::tools::Ask> for Panel {
    /// `ask`'s fifth option -- writing an answer instead of picking one -- is
    /// always available, so it is always the trailing `Text` field rather
    /// than something the tool has to ask for.
    fn from(ask: &crate::tools::Ask) -> Self {
        Panel::new(ask.call_id.clone(), Panel::fields_for(ask))
    }
}

impl Panel {
    /// The rows one question contributes.
    fn fields_for(ask: &crate::tools::Ask) -> Vec<Field> {
        let mut fields = Vec::new();
        // Only the first field carries the question -- it is the panel's
        // heading, not a per-field label, but `Field` has no separate slot
        // for one, so it rides on whichever field is drawn first.
        if !ask.options.is_empty() {
            fields.push(Field::Choice {
                prompt: ask.question.clone(),
                options: ask.options.clone(),
                multi: ask.multiple,
            });
            fields.push(Field::Text {
                prompt: "type an answer".to_owned(),
            });
        } else {
            fields.push(Field::Text {
                prompt: ask.question.clone(),
            });
        }
        fields
    }

    /// Add another call's question to a panel already on screen.
    ///
    /// Appended rather than refused: two questions asked in one turn belong on
    /// one form. Appending also leaves every existing row where it was, so a
    /// highlight the user has already moved does not jump under them.
    pub fn extend_with(&mut self, ask: &crate::tools::Ask) {
        for field in Panel::fields_for(ask) {
            self.state.push(match &field {
                Field::Choice { options, .. } => FieldState::Choice(vec![false; options.len()]),
                Field::Text { .. } => FieldState::Text {
                    text: String::new(),
                    cursor: 0,
                },
            });
            self.owners.push(ask.call_id.clone());
            self.fields.push(field);
        }
    }

    /// One answer per call, in the order the questions were asked.
    ///
    /// Grouped by owner rather than returned per field, because a call is
    /// answered once however many rows it put on screen -- a choice and its
    /// free-text fallback are two ways to answer one question, not two answers.
    pub fn submit_by_call(&self) -> Vec<(String, String)> {
        let per_field = match self.submit() {
            PanelOutcome::Submitted(answers) => answers,
            PanelOutcome::Cancelled => return Vec::new(),
        };
        let mut out: Vec<(String, String)> = Vec::new();
        for (owner, answer) in self.owners.iter().zip(per_field) {
            match out.iter_mut().find(|(id, _)| id == owner) {
                // First non-empty wins: a chosen option beats the empty text
                // field beneath it, and vice versa.
                Some((_, existing)) if existing.is_empty() => *existing = answer,
                Some(_) => {}
                None => out.push((owner.clone(), answer)),
            }
        }
        out
    }

    /// How many heading lines the panel draws: one per question with options.
    pub fn headings(&self) -> usize {
        self.fields
            .iter()
            .filter(|f| matches!(f, Field::Choice { prompt, .. } if !prompt.is_empty()))
            .count()
    }

    /// Every call this panel is answering.
    pub fn calls(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for owner in &self.owners {
            if !out.contains(owner) {
                out.push(owner.clone());
            }
        }
        out
    }
}

/// One conversation column. A fork adds a pane rather than replacing state,
/// so both branches stay live -- which is exactly what the API's `stream_id`
/// lanes are for.
pub struct Pane {
    /// Rendered transcript. Deltas append to the last entry rather than
    /// pushing a new one, so streaming text grows a paragraph in place.
    pub transcript: Vec<Entry>,
    pub input: String,
    /// The transport's state. Private, with `status()` deriving from it and
    /// `outstanding`, so a pane cannot be told it is idle by anything that
    /// does not know whether a request is outstanding.
    link: Link,
    /// Requests Kobold is still waiting on.
    ///
    /// Not "requests in flight on the wire". An interrupted request is
    /// abandoned -- Kobold has stopped waiting on it and discards what comes
    /// back -- so it stops counting the moment the user says so, and the
    /// terminal event that arrives later is the tail of something already
    /// thrown away. Kobold already treats an interrupted response this way
    /// one layer up, where it drops the response id because the answer no
    /// longer describes what is on screen.
    outstanding: usize,
    pub lane: String,
    pub last_response_id: Option<String>,
    /// Lines scrolled up from the bottom. 0 means pinned to newest.
    pub scroll: u16,
    /// Index into `transcript` of the highlighted entry, in Chat mode.
    pub selected: Option<usize>,
    /// Position while walking history; `None` means "at the live draft".
    pub hist: Option<usize>,
    /// The half-typed line set aside when history navigation started.
    pub draft: String,
    /// Caret position in `input`, counted in characters rather than bytes so
    /// multi-byte input cannot land the caret mid-codepoint.
    pub cursor: usize,
    /// Transcript-tree identity. Distinct from `lane`, which is the API's
    /// `stream_id`: a rewind starts a new branch while keeping its lane.
    pub branch: String,
    /// Where this branch was cut from its parent, as (branch, message count).
    pub parent: Option<(String, usize)>,
    /// Records already written for this branch; the next record's `seq`.
    pub logged: usize,
    /// Tool calls the model has opened and not yet closed, by arrival order.
    ///
    /// AG-UI splits one call across `TOOL_CALL_START`, any number of
    /// `TOOL_CALL_ARGS` and `TOOL_CALL_END`, where this provider hands the
    /// whole thing over in a single event. The pieces have to be held
    /// somewhere until the boundary arrives, and it is per pane because a
    /// call belongs to the lane that made it.
    ///
    /// A `Vec` rather than a map: a turn opens one or two of these, lookup is
    /// by id on a list that short, and the arrival order is worth keeping.
    pub open_calls: Vec<PendingCall>,
    /// Messages typed while a turn was in flight, sent in order once it ends.
    pub queue: Vec<String>,
    /// Splits streamed model text into speakable clauses.
    pub chunker: crate::tts::Chunker,
    /// Index of the model entry currently being streamed into. Without this,
    /// anything pushed mid-turn ends the entry and the rest of the reply
    /// appears as a separate block.
    pub open_model: Option<usize>,
    /// Last turn failed. The pane returns to Ready either way, so the status
    /// alone cannot distinguish a failure from a normal finish.
    pub failed: bool,
    /// Set by an interrupt: deltas for the in-flight response are dropped from
    /// here on. The server keeps generating regardless -- there is no working
    /// client-side cancel in WebSocket mode -- so this is a local stop.
    pub interrupted: bool,
    /// Tokens the server saw at the end of the last completed turn: what it
    /// was sent plus what it produced, which is what the next turn starts
    /// from. Not a running total -- each turn's input already counts the whole
    /// history, so summing them would count the same conversation once per
    /// turn.
    pub context: Option<u32>,
    /// A question from a tool, waiting on this pane's panel, and the panel it
    /// is waiting on.
    ///
    /// Per pane rather than per app: a fork is a separate conversation with its
    /// own turn in flight, so a question parked in one must not silence the
    /// prompt in another. Moving to a split with no question outstanding gives
    /// its prompt back.
    /// Questions waiting on this pane's panel. More than one when the model
    /// asked several things in a turn, which the API permits and models do.
    pub pending_asks: Vec<crate::tools::Ask>,
    /// The bottom form panel, open while an `ask` call or an MCP elicitation is
    /// parking this pane's turn on a user answer. `Some` here is what blocks --
    /// there is no separate "waiting for input" status, because the panel's
    /// presence already says it, and it is what hides this pane's prompt.
    pub panel: Option<Panel>,
    /// Row span of each transcript entry, rebuilt each frame by
    /// `render_transcript`'s first pass. Kept on the pane rather than
    /// allocated per frame: it is one element per entry and the contents are
    /// replaced wholesale, so reusing the buffer costs nothing and saves an
    /// allocation at keystroke cadence.
    ranges: Vec<(usize, usize)>,
    /// The geometry and scroll offset of the last frame, so the next one can
    /// tell whether the viewport merely slid. Reset implicitly by a resize,
    /// because the stored `Rect` no longer matches.
    last_paint: Option<(Rect, u16)>,
}

impl Pane {
    /// What this pane is doing. Derived, never stored.
    ///
    /// The invariant this exists to make unrepresentable: **`Waiting`
    /// belongs to whoever has a request outstanding to the adapter, and only
    /// a terminal update for that request clears it.** Four bugs came from
    /// that being a field anything could assign -- a queued `Connected`
    /// marking a running turn idle, an answered question doing the same, and
    /// an automatic tool result that could not be fixed at all because the
    /// `Completed` for the response that requested it arrived afterwards and
    /// overwrote the flag.
    pub fn status(&self) -> Status {
        match self.link {
            // A dead transport is the more important fact, and nothing is
            // outstanding once it dies -- `Disconnected` abandons them.
            Link::Gone(_) => Status::Gone,
            // Before the link's own state, because a request sent while the
            // adapter is still connecting is genuinely outstanding: the user
            // typed before `Connected` arrived, which is an ordinary thing
            // to do at startup and exactly the window the first of these
            // bugs lived in.
            _ if self.outstanding > 0 => Status::Waiting,
            Link::Connecting => Status::Connecting,
            Link::Up => Status::Ready,
        }
    }

    /// Why the connection went away, for the one line that shows it.
    pub fn gone_reason(&self) -> Option<&str> {
        match &self.link {
            Link::Gone(why) => Some(why),
            _ => None,
        }
    }

    /// Kobold has sent a request and is waiting on it.
    pub fn sent_request(&mut self) {
        self.outstanding += 1;
    }

    /// A terminal update arrived for one of them.
    ///
    /// Saturating, because a request can be abandoned before its terminal
    /// event arrives -- an interrupt zeroes the count and the tail still
    /// turns up. Underflowing there would wrap to a pane that is waiting
    /// forever on requests that finished long ago.
    fn request_settled(&mut self) {
        self.outstanding = self.outstanding.saturating_sub(1);
    }

    /// Nothing is outstanding any more, because Kobold has stopped waiting.
    fn abandon_requests(&mut self) {
        self.outstanding = 0;
    }

    fn new(lane: &str, branch: &str) -> Self {
        Self {
            transcript: Vec::new(),
            input: String::new(),
            pending_asks: Vec::new(),
            open_calls: Vec::new(),
            panel: None,
            ranges: Vec::new(),
            link: Link::Connecting,
            outstanding: 0,
            lane: lane.to_owned(),
            last_response_id: None,
            scroll: 0,
            selected: None,
            hist: None,
            draft: String::new(),
            cursor: 0,
            branch: branch.to_owned(),
            parent: None,
            logged: 0,
            queue: Vec::new(),
            chunker: crate::tts::Chunker::default(),
            open_model: None,
            failed: false,
            interrupted: false,
            context: None,
            last_paint: None,
        }
    }

    /// Byte offset of character index `at`.
    // The prompt and the panel's free-text field take the same keys, so they
    // share one implementation. See `crate::edit` for why it is free functions
    // rather than a type these both embed.
    pub fn insert(&mut self, c: char) {
        crate::edit::insert(&mut self.input, &mut self.cursor, c);
    }

    pub fn insert_str(&mut self, text: &str) {
        crate::edit::insert_str(&mut self.input, &mut self.cursor, text);
    }

    pub fn backspace(&mut self) {
        crate::edit::backspace(&mut self.input, &mut self.cursor);
    }

    pub fn delete_forward(&mut self) {
        crate::edit::delete_forward(&mut self.input, &mut self.cursor);
    }

    pub fn move_left(&mut self, word: bool) {
        crate::edit::move_left(&self.input, &mut self.cursor, word);
    }

    pub fn move_right(&mut self, word: bool) {
        crate::edit::move_right(&self.input, &mut self.cursor, word);
    }

    pub fn home(&mut self) {
        crate::edit::home(&mut self.cursor);
    }

    pub fn end(&mut self) {
        crate::edit::end(&self.input, &mut self.cursor);
    }

    pub fn delete_word_back(&mut self) {
        crate::edit::delete_word_back(&mut self.input, &mut self.cursor);
    }

    pub fn kill_to_start(&mut self) {
        crate::edit::kill_to_start(&mut self.input, &mut self.cursor);
    }

    pub fn kill_to_end(&mut self) {
        crate::edit::kill_to_end(&mut self.input, &self.cursor);
    }

    pub fn set_input(&mut self, text: String) {
        self.input = text;
        self.end();
    }

    /// Input laid out for display, plus the caret's (row, col) within it.
    ///
    /// Hard wrap rather than word wrap: the caret has to land on an exact cell,
    /// and word wrapping consumes spaces, which makes character index to screen
    /// position ambiguous.
    pub fn layout_input(&self, width: usize) -> (Vec<String>, (usize, usize)) {
        let width = width.max(1);
        let mut rows: Vec<String> = Vec::new();
        let mut caret = (0usize, 0usize);
        // Character index of the start of the logical line being laid out.
        // `cursor` counts characters, so the caret is found by comparing against
        // running character counts even though the columns are cells.
        let mut seen = 0usize;

        for logical in self.input.split('\n') {
            let mut row = String::new();
            let mut used = 0usize;
            let mut idx = 0usize;
            // Each assignment moves the caret to the last cluster boundary at or
            // before the cursor, so a cursor resting between the parts of one
            // cluster shows up on the cluster rather than nowhere.
            if self.cursor >= seen {
                caret = (rows.len(), 0);
            }
            for g in logical.graphemes(true) {
                let w = crate::md::width_of(g).max(1);
                if used + w > width && used > 0 {
                    rows.push(std::mem::take(&mut row));
                    used = 0;
                }
                row.push_str(g);
                used += w;
                idx += g.chars().count();
                if self.cursor >= seen + idx {
                    caret = (rows.len(), used);
                }
            }
            rows.push(row);
            // One extra row when the line exactly fills the last one, so a caret
            // sitting at the very end has somewhere to be.
            if used == width {
                rows.push(String::new());
                if caret == (rows.len() - 2, width) {
                    caret = (rows.len() - 1, 0);
                }
            }
            seen += logical.chars().count() + 1;
        }
        (rows, caret)
    }

    /// Messages that count toward a branch point: system notes are chrome and
    /// are never inherited.
    pub fn message_count(&self) -> usize {
        self.transcript
            .iter()
            .filter(|e| e.who != Who::System)
            .count()
    }

    pub fn cursor(&self) -> crate::transcript::Cursor<'_> {
        crate::transcript::Cursor {
            branch: &self.branch,
            seq: self.logged,
            parent: self.parent.as_ref().map(|(b, n)| (b.as_str(), *n)),
        }
    }

    /// Everything needed to restart a chain from scratch. Used after a rewind
    /// or a fork, where `previous_response_id` points at a response the
    /// connection-local cache may already have evicted.
    pub fn replay(&self) -> Vec<(String, String)> {
        self.transcript
            .iter()
            .filter(|e| e.who != Who::System)
            .map(|e| {
                let role = if e.who == Who::User {
                    "user"
                } else {
                    "assistant"
                };
                (role.to_owned(), e.text.clone())
            })
            .collect()
    }
}

pub struct App {
    pub panes: Vec<Pane>,
    pub active: usize,
    pub should_quit: bool,
    pub should_detach: bool,
    /// Ctrl+D once arms, twice quits. Any other key disarms.
    pub quit_armed: bool,
    /// Esc once arms an interrupt, twice fires it. Any other key disarms.
    pub esc_armed: bool,
    /// Toggled by `/voice`. Off by default: speech needs a listener on the
    /// other end of the SSH tunnel, which may not be running.
    pub voice: bool,
    /// Whether an engine command is configured at all, so `/voice` can say so
    /// instead of silently doing nothing.
    pub voice_engine: bool,
    /// Audio is shipped to another machine rather than played here.
    pub voice_remote: bool,
    /// Clauses produced this update, drained by the caller into the engine.
    /// Kept as data so this module never touches IO.
    pub speech: Vec<String>,
    /// Set when speech in progress should be dropped.
    pub speech_cancel: bool,
    /// Transient engine status, shown in the bar. Not the transcript: these are
    /// about the machinery, and interleaving them with a streaming reply both
    /// buries the reply and breaks it into fragments.
    pub notice: Option<(String, std::time::Instant)>,
    /// Advances only while waiting, so an idle screen never repaints.
    pub spinner: usize,
    pub mode: Mode,
    /// Shared across panes: history is about what you typed, not where.
    pub history: Vec<String>,
    /// A question from a tool, waiting for the person at the keyboard, with
    /// the lane whose turn is parked on it.
    ///
    /// One slot rather than a queue: the panel covers the prompt, so a second
    /// question arriving while one is open has nowhere to go. The model is
    /// told to ask again rather than the questions silently stacking up behind
    /// a panel that only shows the first.
    /// `--debug`: show the raw tool traffic in the transcript.
    ///
    /// Off by default because the conversation is the thing being read, and a
    /// call id with a blob of JSON in the middle of it is machinery. On, it is
    /// the fastest way to see exactly what the model asked for.
    pub debug: bool,
    /// Shown in the bar, so which model and how hard it is thinking is never
    /// a guess about what some config file said.
    pub model: String,
    pub effort: String,
    pub harness: String,
    /// What the model can hold, for the gauge. Zero means unknown, and the
    /// gauge then reports the count alone rather than a fraction of a number
    /// nobody supplied.
    pub context_window: u32,
    /// 256-colour index for a fenced code block's tinted background. Set
    /// from `settings::Settings::code_bg`, which already folds `KOBOLD_CODE_BG`
    /// in ahead of this -- `md` used to read the environment itself, which
    /// made the settings-file key do nothing.
    pub code_bg: u8,
    /// The highlighted slash-command suggestion, with the input it was chosen
    /// against. Keeping the text alongside the index means a selection stops
    /// applying the instant the line changes, so no editing key has to
    /// remember to reset it and none can forget.
    menu: Option<(String, usize)>,
    /// Monotonic, so a closed pane's lane id is never reused on the same
    /// connection while the server still knows about it.
    lane_seq: usize,
    root_branch: String,
    pub root: Option<std::path::PathBuf>,
}

pub struct Entry {
    pub who: Who,
    pub text: String,
    /// Set on model entries when their turn completes. Kept per entry, not
    /// just per pane, so a rewind knows which response it is rewinding to.
    pub response_id: Option<String>,
    /// Memoised layout, and the (width, text length) it was built from.
    ///
    /// Laying an entry out means parsing markdown, lexing code and measuring
    /// tables, none of which is cheap. Without this the whole transcript is
    /// redone on every frame, so a keystroke costs O(session) and a long
    /// conversation gets slower without bound.
    ///
    /// Length is a sound version key because `text` is only ever appended to
    /// (one site: the delta handler in `apply`) and never rewritten in place.
    lines: Vec<Line<'static>>,
    key: Option<(usize, usize)>,
    /// Where the next append can resume laying out from, for markdown that has
    /// only ever grown. Without it a delta re-parses the whole reply, so one
    /// answer costs O(n^2) in its own length.
    resume: Option<crate::md::Resume>,
}

impl Entry {
    fn new(who: Who, text: String) -> Entry {
        Entry {
            who,
            text,
            response_id: None,
            lines: Vec::new(),
            key: None,
            resume: None,
        }
    }

    fn clone_of(e: &Entry) -> Entry {
        // The layout is deliberately not copied: the clone is about to be laid
        // out again at whatever width its new pane has.
        Entry {
            who: e.who,
            text: e.text.clone(),
            response_id: e.response_id.clone(),
            lines: Vec::new(),
            key: None,
            resume: None,
        }
    }

    /// Lay the entry out, reusing the last result when nothing that affects it
    /// has changed. A streaming delta only ever dirties the newest entry.
    ///
    /// `code_bg` is not part of the cache key: it is set once from settings at
    /// startup and never changes for the life of the session, unlike `width`.
    fn refresh(&mut self, width: usize, code_bg: u8) -> crate::layout::Outcome {
        if self.key == Some((width, self.text.len())) {
            return crate::layout::Outcome::Hit;
        }
        // Streaming append at an unchanged width: every block above the one
        // still being written is already final, so only that block is rebuilt.
        // Anything else -- a resize, a first render, text that did not simply
        // grow -- falls through to laying the whole entry out.
        // Taking rather than cloning is safe: the only way past here is the
        // full render below, which replaces it outright.
        if let (Some((w, len)), Some(at)) = (self.key, self.resume.take()) {
            if w == width && self.text.len() > len && at.at <= len {
                self.lines.truncate(at.lines);
                let (tail, next) = crate::md::render_from(&self.text, width, at, code_bg);
                self.lines.extend(tail);
                self.resume = Some(next);
                self.key = Some((width, self.text.len()));
                return crate::layout::Outcome::Resumed;
            }
        }
        let (lines, resume) = layout_entry(self.who, &self.text, width, code_bg);
        self.lines = lines;
        self.resume = resume;
        self.key = Some((width, self.text.len()));
        crate::layout::Outcome::Full
    }
}

/// Overlay applied to a row as it is written: the pane-focus fade and the
/// browsed-entry highlight. Kept as a description rather than mutated onto a
/// cloned `Line`, so a row costs no allocation to style.
///
/// `dim` and `bg`/`bold` are mutually exclusive in practice -- a pane is either
/// the active one, where a selection can exist, or an inactive one that fades
/// -- so their order here does not matter.
#[derive(Clone, Copy, Default)]
struct Tint {
    /// An unfocused pane recedes. DIM alone is not enough -- some terminals
    /// ignore SGR 2 -- so default-coloured prose also drops to grey, which is
    /// visible everywhere. Coloured spans keep their hue and just fade.
    dim: bool,
    bg: Option<Color>,
    /// Set on the browsed entry's own rows, not on its blank padding.
    bold: bool,
}

impl Tint {
    fn apply(self, mut style: Style) -> Style {
        if self.dim {
            style = style.add_modifier(Modifier::DIM);
            if style.fg.is_none() {
                style = style.fg(Color::Indexed(244));
            }
        }
        if let Some(bg) = self.bg {
            style = style.bg(bg);
        }
        if self.bold {
            style = style.add_modifier(Modifier::BOLD);
            // Lift default-coloured prose to full white; text that already
            // carries a colour keeps it.
            if style.fg.is_none() {
                style = style.fg(Color::Indexed(255));
            }
        }
        style
    }
}

/// Write one already-wrapped line into the buffer, clipped at `right`, and
/// return the column it stopped at.
///
/// `Paragraph` is not used for this. Its line composer re-segments graphemes
/// and re-measures widths in order to decide where to break -- work we already
/// did when the entry was laid out, and which it then has to redo on every
/// frame. Measured at 30 rows: `Paragraph` 98us, this 38us.
///
/// The fast path is a byte loop, valid only when every byte is printable ASCII,
/// where one byte is one grapheme one column wide. Anything else -- box
/// drawing, the prompt marker, CJK, emoji, combining marks -- takes the slow
/// path, which is a faithful copy of what `Paragraph` does, down to skipping
/// zero-width graphemes and substituting a space for an empty symbol.
fn blit(buf: &mut Buffer, x: u16, y: u16, right: u16, line: &Line, tint: Tint) -> u16 {
    let mut x = x;
    if line
        .spans
        .iter()
        .all(|s| s.content.bytes().all(|b| (0x20..0x7f).contains(&b)))
    {
        for span in &line.spans {
            let style = tint.apply(line.style.patch(span.style));
            for b in span.content.bytes() {
                if x >= right {
                    return x;
                }
                buf[(x, y)].set_char(b as char).set_style(style);
                x += 1;
            }
        }
        return x;
    }
    for g in line.styled_graphemes(Style::default()) {
        let w = g.symbol.cell_width();
        if w == 0 {
            continue;
        }
        if x + w > right {
            break;
        }
        // Overwrite whatever was here with a space rather than a zero-width.
        let symbol = if g.symbol.is_empty() { " " } else { g.symbol };
        buf[(x, y)]
            .set_symbol(symbol)
            .set_style(tint.apply(g.style));
        x += w;
    }
    x
}

/// `18`, `1.2k`, `340k` -- short enough for a status bar, where the order of
/// magnitude matters more than the exact count.
fn thousands(n: u32) -> String {
    match n {
        0..=9_999 => n.to_string(),
        _ => format!("{:.0}k", n as f64 / 1000.0),
    }
}

/// Fill `[x, right)` on one row with a single styled blank.
fn fill(buf: &mut Buffer, x: u16, y: u16, right: u16, style: Style) {
    for cx in x..right {
        buf[(cx, y)].set_char(' ').set_style(style);
    }
}

/// What a rendered frame wants from the terminal beyond its cells.
#[derive(Default)]
pub struct Painted {
    /// Where to leave the caret, or `None` to hide it.
    pub cursor: Option<Position>,
    /// The transcript scrolled by this many rows within this row range, and
    /// nothing else in the range moved. Lets the screen ask the terminal to
    /// scroll instead of repainting every row.
    ///
    /// Strictly a hint. The screen still diffs afterwards, so a wrong hint
    /// costs the bytes we were trying to save and nothing else -- correctness
    /// rests only on the in-memory shift matching what the escape does.
    /// Positive scrolls content up, which is what appending to the transcript
    /// does; negative is scrolling back through history.
    pub scroll: Option<(core::ops::Range<u16>, i32)>,
}

/// Columns available to entry text. One column is reserved for the leading pad
/// added at draw time, so wrapping to the full area width would push the last
/// character off the right edge.
fn transcript_width(area: Rect) -> usize {
    (area.width as usize).saturating_sub(1).max(8)
}

/// One entry's lines, independent of where it lands on screen. Selection,
/// dimming and the pad column are applied later, to the visible rows only.
///
/// Model entries also report where a later append can resume from. Nothing else
/// streams -- a user message and a system note arrive whole -- so they do not.
fn layout_entry(
    who: Who,
    text: &str,
    width: usize,
    code_bg: u8,
) -> (Vec<Line<'static>>, Option<crate::md::Resume>) {
    let dim = Style::default().fg(Color::DarkGray);
    // Model output is markdown; user input and system notes are not, so they
    // render literally.
    if who == Who::Model {
        let (lines, resume) =
            crate::md::render_from(text, width, crate::md::Resume::default(), code_bg);
        return (lines, Some(resume));
    }
    let style = if who == Who::User {
        Style::default().fg(Color::Cyan)
    } else {
        dim
    };
    let mut out = Vec::new();
    for (i, raw) in text.split('\n').enumerate() {
        // Only the user's first line carries a marker. Everything else starts
        // flush at the margin, because a prefix on a continuation line would
        // make wrapped text hang left of where it started.
        let prefix = if who == Who::User && i == 0 {
            "\u{203a} "
        } else {
            ""
        };
        // Wrapping here, rather than with `Wrap`, because scrolling needs an
        // exact line count.
        for (j, chunk) in wrap(raw, width.saturating_sub(prefix.chars().count()).max(1))
            .into_iter()
            .enumerate()
        {
            let head = if j == 0 { prefix } else { "" };
            out.push(Line::from(vec![
                Span::styled(head, dim),
                Span::styled(chunk, style),
            ]));
        }
    }
    (out, None)
}

/// One row below the transcript proper: the spinner and the queued
/// messages, which belong to no entry and change every frame.
///
/// Only the tail is represented this way. Entry rows used to be too, one
/// `Slot` per row of the whole transcript, which made placing a screenful
/// cost the whole session; they are spans in `Pane::ranges` now. The tail
/// stays materialised because it is bounded by the queue rather than by the
/// transcript.
#[derive(Clone, Copy)]
enum Slot {
    Blank,
    /// Index into the per-frame extras: spinner and queued messages, which
    /// are not part of any entry.
    Extra(u32),
}

/// The entry a layout row belongs to, as `(entry index, line within it)`.
///
/// `None` for a separator row, which belongs to neither of the entries it
/// sits between. Binary search rather than a scan: the spans are sorted and
/// non-overlapping by construction, and the alternative to searching is the
/// materialised per-row vector this replaced.
fn entry_at(ranges: &[(usize, usize)], r: usize) -> Option<(usize, usize)> {
    // The last span that starts at or before `r`. An empty entry can share a
    // start with the next one, and taking the last is what steps over it.
    let i = ranges.partition_point(|&(start, _)| start <= r);
    let (start, end) = *ranges.get(i.checked_sub(1)?)?;
    (r < end).then_some((i - 1, r - start))
}

/// Where display row `r` lands once the two blank rows that bracket the
/// browsed entry are accounted for.
///
/// The blanks are not inserted into anything. Inserting them costs an O(n)
/// shift of every row after them, twice, which is exactly the per-frame work
/// the placement pass exists to avoid -- and they only ever bracket one
/// contiguous span, so the effect on every other row is a constant shift.
///
/// `a` and `b` are the selected entry's half-open row span *before* the
/// blanks. `None` means this row is one of the blanks and has nothing behind
/// it to draw.
///
/// Split out as a free function rather than left inline because it is pure
/// arithmetic over three numbers, which makes it directly testable -- and
/// because off-by-one arithmetic in this file is precisely what the mutation
/// survivors have always been.
fn unblank(r: usize, a: usize, b: usize) -> Option<usize> {
    if r < a {
        Some(r)
    } else if r == a {
        None
    } else if r <= b {
        Some(r - 1)
    } else if r == b + 1 {
        None
    } else {
        Some(r - 2)
    }
}

pub use kobold_core::lane::Who;

/// How long an engine notice stays in the bar before the key hints return.
const NOTICE_TTL: std::time::Duration = std::time::Duration::from_secs(6);

const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

/// Greedy word wrap. A word longer than the width is hard-split rather than
/// allowed to overflow, so a long URL or path cannot break the layout.
fn wrap(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    // Measured in cells rather than chars, so wide text breaks where the
    // painter will actually run out of room. See `md::width_of`.
    let cells = crate::md::width_of;
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split(' ') {
        let mut word = word;
        while cells(word) > width {
            if !line.is_empty() {
                out.push(std::mem::take(&mut line));
            }
            let split = crate::md::split_at_width(word, width);
            out.push(word[..split].to_owned());
            word = &word[split..];
        }
        let need = cells(word) + usize::from(!line.is_empty());
        if cells(&line) + need > width {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    out.push(line);
    out
}

impl App {
    pub fn new(lane: &str, root_branch: &str) -> Self {
        Self {
            panes: vec![Pane::new(lane, root_branch)],
            active: 0,
            should_quit: false,
            should_detach: false,
            quit_armed: false,
            esc_armed: false,
            voice: false,
            voice_engine: false,
            voice_remote: false,
            speech: Vec::new(),
            speech_cancel: false,
            notice: None,
            spinner: 0,
            mode: Mode::Send,
            history: Vec::new(),
            debug: false,
            model: String::new(),
            effort: String::new(),
            harness: String::new(),
            context_window: 0,
            code_bg: 235,
            menu: None,
            lane_seq: 0,
            root_branch: root_branch.to_owned(),
            root: None,
        }
    }

    /// Branch ids are session-scoped so they never collide with a branch from
    /// an earlier run appended to the same file.
    fn next_branch(&mut self) -> String {
        self.lane_seq += 1;
        format!("{}.{}", self.root_branch, self.lane_seq)
    }

    pub fn pane(&self) -> &Pane {
        &self.panes[self.active]
    }

    /// Whether anything has gone wrong enough to report as an error rather
    /// than as progress.
    ///
    /// A dropped transport and a turn the provider rejected are different
    /// facts, and the terminal's progress bar has one state for both, so they
    /// collapse here rather than at the call site.
    pub fn any_pane_failed(&self) -> bool {
        self.panes
            .iter()
            .any(|p| p.status() == Status::Gone || p.failed)
    }

    pub fn pane_mut(&mut self) -> &mut Pane {
        let i = self.active;
        &mut self.panes[i]
    }

    pub fn focus_pane(&mut self, delta: isize) -> bool {
        if self.panes.len() < 2 {
            return false;
        }
        let n = self.panes.len() as isize;
        self.active = ((self.active as isize + delta).rem_euclid(n)) as usize;
        true
    }

    /// Closing the last pane would leave nothing to type into, so the final
    /// pane is kept and the request ignored.
    pub fn close_pane(&mut self) -> bool {
        if self.panes.len() < 2 {
            return false;
        }
        self.panes.remove(self.active);
        self.active = self.active.min(self.panes.len() - 1);
        true
    }

    /// Discard everything after the highlighted entry. A model message stays
    /// and becomes the new tail; a user message is lifted back into the input
    /// so it can be edited and re-sent.
    pub fn rewind(&mut self) -> bool {
        let Some(sel) = self.pane().selected else {
            return false;
        };
        let pane = self.pane_mut();
        let Some(entry) = pane.transcript.get(sel) else {
            return false;
        };
        let who = entry.who;
        let text = entry.text.clone();

        pane.transcript.truncate(sel + 1);
        if who == Who::User {
            pane.transcript.pop();
            pane.set_input(text);
        }
        // The chain restarts from replayed context: the response we would
        // otherwise point at is very likely evicted from the connection cache.
        pane.last_response_id = None;
        // The entry deltas were streaming into is gone, or is no longer the
        // tail. **An index that outlives what it points at is a bug generator
        // rather than a bug**: the delta arm's `Who::Model` guard makes that
        // one consumer safe and does nothing for the next reader, who has no
        // way to know the invariant was ever broken. Truncating a `Vec` while
        // leaving an index into it reads as correct on inspection, which is
        // what makes it worth clearing here rather than defending downstream.
        pane.open_model = None;
        pane.selected = None;
        pane.scroll = 0;

        // A rewind is a fork made in place, so it opens a branch too. Without
        // this the log would keep records the session has discarded, and
        // replaying the file would rebuild a conversation that never happened.
        let cut = pane.message_count();
        let old = pane.branch.clone();
        let new = self.next_branch();
        let pane = self.pane_mut();
        pane.parent = Some((old, cut));
        pane.branch = new;
        pane.logged = 0;

        self.mode = Mode::Send;
        true
    }

    /// Same truncation as a rewind, but into a new pane, leaving the original
    /// branch untouched and still usable.
    pub fn fork(&mut self) -> bool {
        let Some(sel) = self.pane().selected else {
            return false;
        };
        let branch = self.next_branch();
        let lane = format!("fork-{}", self.lane_seq);

        let src = self.pane();
        let Some(entry) = src.transcript.get(sel) else {
            return false;
        };
        let (who, text) = (entry.who, entry.text.clone());

        let mut pane = Pane::new(&lane, &branch);
        pane.transcript = src.transcript[..=sel].iter().map(Entry::clone_of).collect();
        // A fork inherits the transport, not the turn: whatever the original
        // is waiting on is not this pane's.
        pane.link = Link::Up;
        if who == Who::User {
            pane.transcript.pop();
            pane.set_input(text);
        }
        pane.parent = Some((src.branch.clone(), pane.message_count()));

        self.panes.push(pane);
        self.active = self.panes.len() - 1;
        self.panes[0].selected = None;
        for p in self.panes.iter_mut() {
            p.selected = None;
        }
        self.mode = Mode::Send;
        true
    }

    /// Older, then older still. Saves the live draft on the first step so it
    /// can be restored by walking back down past the newest entry.
    pub fn history_prev(&mut self) -> bool {
        if self.history.is_empty() {
            return false;
        }
        let next = match self.pane().hist {
            None => {
                let taken = std::mem::take(&mut self.pane_mut().input);
                self.pane_mut().draft = taken;
                self.history.len() - 1
            }
            Some(0) => return false,
            Some(i) => i - 1,
        };
        let v = self.history[next].clone();
        let p = self.pane_mut();
        p.hist = Some(next);
        p.set_input(v);
        true
    }

    pub fn history_next(&mut self) -> bool {
        match self.pane().hist {
            None => false,
            Some(i) if i + 1 < self.history.len() => {
                let v = self.history[i + 1].clone();
                let p = self.pane_mut();
                p.hist = Some(i + 1);
                p.set_input(v);
                true
            }
            Some(_) => {
                let p = self.pane_mut();
                p.hist = None;
                let d = std::mem::take(&mut p.draft);
                p.set_input(d);
                true
            }
        }
    }

    /// Whether the suggestion list is on screen. It opens on a leading slash
    /// and closes as soon as the input stops naming a command, so there is
    /// nothing to dismiss and nothing to get stuck open.
    pub fn menu_open(&self) -> bool {
        !crate::complete::matches(&self.pane().input).is_empty()
    }

    /// The highlighted suggestion. A selection belongs to the list it was made
    /// in, so one made against other text -- or now out of range because the
    /// list narrowed -- falls back to the first entry.
    pub fn menu_index(&self) -> usize {
        let n = crate::complete::matches(&self.pane().input).len();
        match &self.menu {
            Some((at, i)) if *at == self.pane().input && *i < n => *i,
            _ => 0,
        }
    }

    /// Move the highlight, wrapping at both ends: with three entries, reaching
    /// the last one by pressing up once is the shorter path.
    pub fn menu_move(&mut self, delta: isize) -> bool {
        let n = crate::complete::matches(&self.pane().input).len();
        if n == 0 {
            return false;
        }
        let next = (self.menu_index() as isize + delta).rem_euclid(n as isize) as usize;
        self.menu = Some((self.pane().input.clone(), next));
        true
    }

    /// What `Tab` does, which depends on how far along the line is.
    ///
    /// While the command is still being named it takes the highlighted
    /// suggestion and adds a trailing space -- every command with an argument
    /// wants one, and the ones without are about to be sent anyway. Once the
    /// name is settled the same key steps through the arguments instead, so
    /// one key carries the line from `/` to `/voice marius` without ever
    /// meaning two things at the same moment.
    pub fn complete_slash(&mut self) -> bool {
        let list = crate::complete::matches(&self.pane().input);
        if let Some(cmd) = list.get(self.menu_index()) {
            let text = format!("/{} ", cmd.name);
            self.menu = None;
            self.pane_mut().set_input(text);
            return true;
        }
        match crate::complete::next_arg(&self.pane().input, self.voice) {
            Some(text) => {
                self.pane_mut().set_input(text);
                true
            }
            None => false,
        }
    }

    /// Whether `Tab` would step through arguments rather than complete a name.
    /// Asked by rendering the bar the same way it is answered by pressing the
    /// key, so the two cannot disagree.
    pub fn args_open(&self) -> bool {
        crate::complete::next_arg(&self.pane().input, self.voice).is_some()
    }

    /// Whether the bottom panel is up. While it is, it owns the keyboard --
    /// the turn is parked on an answer, so ordinary typing and sending must
    /// not run underneath it.
    pub fn panel_open(&self) -> bool {
        self.pane().panel.is_some()
    }

    pub fn open_panel(&mut self, panel: Panel) {
        self.pane_mut().panel = Some(panel);
    }

    pub fn panel_move(&mut self, delta: isize) -> bool {
        self.pane_mut()
            .panel
            .as_mut()
            .is_some_and(|p| p.move_row(delta))
    }

    pub fn panel_toggle(&mut self) -> bool {
        self.pane_mut().panel.as_mut().is_some_and(|p| p.toggle())
    }

    pub fn panel_type(&mut self, c: char) -> bool {
        self.pane_mut()
            .panel
            .as_mut()
            .is_some_and(|p| p.type_char(c))
    }

    pub fn panel_char(&mut self, c: char) -> bool {
        self.pane_mut().panel.as_mut().is_some_and(|p| p.char(c))
    }

    pub fn panel_backspace(&mut self) -> bool {
        self.pane_mut()
            .panel
            .as_mut()
            .is_some_and(|p| p.backspace())
    }

    pub fn panel_paste(&mut self, run: &str) -> bool {
        self.pane_mut().panel.as_mut().is_some_and(|p| p.paste(run))
    }

    pub fn panel_delete_forward(&mut self) -> bool {
        self.pane_mut()
            .panel
            .as_mut()
            .is_some_and(|p| p.delete_forward())
    }

    pub fn panel_move_left(&mut self, word: bool) -> bool {
        self.pane_mut()
            .panel
            .as_mut()
            .is_some_and(|p| p.move_left(word))
    }

    pub fn panel_move_right(&mut self, word: bool) -> bool {
        self.pane_mut()
            .panel
            .as_mut()
            .is_some_and(|p| p.move_right(word))
    }

    pub fn panel_home(&mut self) -> bool {
        self.pane_mut().panel.as_mut().is_some_and(|p| p.home())
    }

    pub fn panel_end(&mut self) -> bool {
        self.pane_mut().panel.as_mut().is_some_and(|p| p.end())
    }

    pub fn panel_delete_word_back(&mut self) -> bool {
        self.pane_mut()
            .panel
            .as_mut()
            .is_some_and(|p| p.delete_word_back())
    }

    pub fn panel_kill_to_start(&mut self) -> bool {
        self.pane_mut()
            .panel
            .as_mut()
            .is_some_and(|p| p.kill_to_start())
    }

    pub fn panel_kill_to_end(&mut self) -> bool {
        self.pane_mut()
            .panel
            .as_mut()
            .is_some_and(|p| p.kill_to_end())
    }

    /// What has been typed into the panel's focused text field. For tests and
    /// for the renderer, which needs the text and the caret together.
    pub fn panel_answer_text(&self) -> Option<String> {
        self.pane().panel.as_ref().and_then(|p| p.focused_text())
    }

    /// Esc: close the panel without an answer. `None` if there was no panel
    /// to close, so a stray Esc when nothing is open is a true no-op.
    pub fn cancel_panel(&mut self) -> Option<String> {
        self.pane_mut().panel.take().map(|p| p.token)
    }

    /// Enter: close the panel and report what it collected. `None` if there
    /// was no panel open.
    /// Close the panel and report an answer per call it was carrying.
    ///
    /// A list because one panel can be answering several questions at once, and
    /// each of them is a separate tool call needing its own result. Empty when
    /// there was no panel.
    pub fn submit_panel(&mut self) -> Vec<(String, String)> {
        match self.pane_mut().panel.take() {
            Some(panel) => panel.submit_by_call(),
            None => Vec::new(),
        }
    }

    /// Close the panel without answering, reporting every call it was carrying
    /// so each turn is released rather than left waiting on a panel that is
    /// no longer there.
    pub fn cancel_panel_calls(&mut self) -> Vec<String> {
        match self.pane_mut().panel.take() {
            Some(panel) => panel.calls(),
            None => Vec::new(),
        }
    }

    /// Match a panel's token against `pending_ask` and, on a match, take it
    /// -- returning `(lane, call_id)` for the caller to answer with a
    /// `ToolResult`. `None` on a mismatch (or nothing pending), and in that
    /// case `pending_ask` is left exactly as it was: answering a stale token
    /// must never send a result for the wrong call, and taking the real
    /// question first would strand its turn with nothing left to answer it.
    /// The active pane's panel, for tests and anything that needs to look at
    /// what is on screen without knowing which pane is showing it.
    pub fn panel(&self) -> Option<&Panel> {
        self.pane().panel.as_ref()
    }

    /// Put an answered question into the conversation as ordinary messages.
    ///
    /// The question becomes an assistant message and the answer a user one,
    /// posted together after the panel is submitted rather than when the call
    /// arrives. Asking is something the assistant did and answering is
    /// something the user did, so the transcript reads as what happened
    /// instead of as the plumbing that carried it.
    ///
    /// After, not before: a question shown while it is still on screen would
    /// appear twice, and one that is cancelled never happened as far as the
    /// conversation is concerned.
    /// Mark the pane running `lane` as having a request outstanding.
    ///
    /// `Waiting` belongs to whoever has a request outstanding to the
    /// adapter, and only a terminal update for that request clears it. A
    /// tool result is such a request -- the model answers it -- so the pane
    /// is waiting again from the moment the answer goes out.
    ///
    /// By lane rather than by whichever pane is focused: the user may have
    /// moved to another split before answering, and the turn belongs to the
    /// conversation it happened in.
    pub fn sent_request(&mut self, lane: &str) {
        if let Some(pane) = self.panes.iter_mut().find(|p| p.lane == lane) {
            pane.sent_request();
        }
    }

    pub fn record_answer(&mut self, lane: &str, question: &str, answer: &str) {
        let Some(idx) = self.panes.iter().position(|p| p.lane == lane) else {
            return;
        };
        let question = question.trim();
        if !question.is_empty() {
            self.panes[idx]
                .transcript
                .push(Entry::new(Who::Model, question.to_owned()));
        }
        let answer = answer.trim();
        if !answer.is_empty() {
            self.panes[idx]
                .transcript
                .push(Entry::new(Who::User, answer.to_owned()));
        }
        self.panes[idx].scroll = 0;
    }

    /// Whether the pane running `lane` already has a question parked on it.
    /// Asked per lane rather than globally: a question in one split says
    /// nothing about whether another split can take one.
    pub fn ask_parked_on(&self, lane: &str) -> bool {
        self.panes
            .iter()
            .any(|p| p.lane == lane && !p.pending_asks.is_empty())
    }

    /// Park a question on the pane running `lane`. Silently dropped when no
    /// pane owns the lane, which happens when a split is closed mid-turn --
    /// the answer would have nowhere to go and nobody to ask.
    pub fn park_ask(&mut self, lane: &str, ask: crate::tools::Ask) {
        let Some(pane) = self.panes.iter_mut().find(|p| p.lane == lane) else {
            return;
        };
        match &mut pane.panel {
            // Already asking: this belongs on the same form.
            Some(panel) => panel.extend_with(&ask),
            None => pane.panel = Some(Panel::from(&ask)),
        }
        pane.pending_asks.push(ask);
    }

    pub fn resolve_pending_ask(&mut self, token: &str) -> Option<(String, crate::tools::Ask)> {
        // Searched by token across panes rather than taken from the active one:
        // the answer belongs to the question it was asked for, and the user may
        // have moved to another split before submitting.
        let pane = self
            .panes
            .iter_mut()
            .find(|p| p.pending_asks.iter().any(|a| a.call_id == token))?;
        let at = pane.pending_asks.iter().position(|a| a.call_id == token)?;
        // Removed one at a time: a panel carrying two questions resolves each
        // separately, and the others must stay parked until they are answered.
        let ask = pane.pending_asks.remove(at);
        Some((pane.lane.clone(), ask))
    }

    /// Only user and model entries are selectable; system notes are chrome.
    fn selectable(&self) -> Vec<usize> {
        self.pane()
            .transcript
            .iter()
            .enumerate()
            .filter(|(_, e)| e.who != Who::System)
            .map(|(i, _)| i)
            .collect()
    }

    pub fn select_prev(&mut self) -> bool {
        let items = self.selectable();
        if items.is_empty() {
            return false;
        }
        let pos = match self.pane().selected {
            None => items.len() - 1,
            Some(cur) => match items.iter().position(|&i| i == cur) {
                Some(0) | None => return false,
                Some(p) => p - 1,
            },
        };
        self.pane_mut().selected = Some(items[pos]);
        self.mode = Mode::Chat;
        true
    }

    /// Walking down past the last entry hands focus back to the prompt, which
    /// is the only way out of Chat mode besides Esc.
    pub fn select_next(&mut self) -> bool {
        let items = self.selectable();
        let Some(cur) = self.pane().selected else {
            return false;
        };
        match items.iter().position(|&i| i == cur) {
            Some(p) if p + 1 < items.len() => {
                self.pane_mut().selected = Some(items[p + 1]);
                true
            }
            _ => {
                self.leave_chat();
                true
            }
        }
    }

    /// Stop showing the in-flight answer. Local only: the request keeps
    /// running and billing server-side.
    pub fn interrupt(&mut self) -> bool {
        if self.pane().status() != Status::Waiting {
            return false;
        }
        self.speech_cancel = true;
        let pane = self.pane_mut();
        pane.chunker.flush();
        pane.interrupted = true;
        // Abandoned, not merely finished. Kobold has stopped waiting on the
        // response, so it is no longer outstanding -- and the terminal event
        // the server still sends is the tail of something already discarded.
        pane.abandon_requests();
        pane.transcript
            .push(Entry::new(Who::System, "interrupted".to_owned()));
        true
    }

    /// Put every queued message back where it came from, one per line, so the
    /// boundaries between separate messages survive the round trip.
    pub fn unqueue(&mut self) -> bool {
        if self.pane().queue.is_empty() {
            return false;
        }
        let pane = self.pane_mut();
        let mut text = pane.queue.join("\n");
        pane.queue.clear();
        if !pane.input.is_empty() {
            text.push('\n');
            text.push_str(&pane.input);
        }
        pane.set_input(text);
        true
    }

    pub fn leave_chat(&mut self) {
        let pane = self.pane_mut();
        pane.selected = None;
        pane.scroll = 0;
        self.mode = Mode::Send;
    }

    pub fn set_notice(&mut self, text: impl Into<String>) {
        self.notice = Some((text.into(), std::time::Instant::now()));
    }

    /// A notice is worth a repaint only while it is on screen.
    pub fn notice_live(&self) -> bool {
        self.notice
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() < NOTICE_TTL)
    }

    pub fn push(&mut self, who: Who, text: impl Into<String>) {
        let pane = self.pane_mut();
        pane.transcript.push(Entry::new(who, text.into()));
        pane.scroll = 0;
    }

    /// Route by lane. A forked pane runs its own `stream_id`, so events from
    /// both branches interleave on the one connection and must be dispatched,
    /// not assumed to belong to whatever is focused.
    /// A connection change, which is true of every pane at once: there is one
    /// socket behind all of them, whichever lane is focused.
    ///
    /// Separate from `apply` rather than a variant of it, because these
    /// carry no lane and routing them by one was a real bug. They used to be
    /// caught by an early return before the lane lookup; now the type says
    /// it.
    pub fn apply_transport(&mut self, transport: Transport) {
        match transport {
            Transport::Connected => {
                // Only the transport's own state, and nothing about any
                // turn. Whether a pane is waiting is derived from what it
                // has outstanding, so there is nothing here to get wrong --
                // the special case this used to need disappeared with the
                // field.
                for p in self.panes.iter_mut() {
                    p.link = Link::Up;
                }
            }
            Transport::Disconnected(why) => {
                for p in self.panes.iter_mut() {
                    p.link = Link::Gone(why.clone());
                    // Nothing outstanding survives a dropped transport: no
                    // terminal event is ever coming for it, so a count kept
                    // here would leak and a reconnect would start dirty.
                    p.abandon_requests();
                }
            }
        }
    }

    /// Sets the status of a lane based on a northbound status change.
    pub fn set_lane_status(&mut self, lane: &str, status: kobold_proto::northbound::LaneStatus) {
        if let Some(pane) = self.panes.iter_mut().find(|p| p.lane == lane) {
            match status {
                kobold_proto::northbound::LaneStatus::Connecting => {
                    pane.link = Link::Connecting;
                    pane.outstanding = 0;
                }
                kobold_proto::northbound::LaneStatus::Ready => {
                    pane.link = Link::Up;
                    pane.outstanding = 0;
                }
                kobold_proto::northbound::LaneStatus::Waiting => {
                    pane.link = Link::Up;
                    pane.outstanding = 1;
                }
                kobold_proto::northbound::LaneStatus::Gone => {
                    pane.link = Link::Gone("connection lost".to_string());
                    pane.outstanding = 0;
                }
            }
        }
    }

    /// Hydrates a lane from a northbound Snapshot frame received from the kernel daemon.
    pub fn apply_snapshot(
        &mut self,
        lane_name: &str,
        branch_name: &str,
        messages: &[kobold_proto::northbound::TranscriptRecord],
        active_interrupt: Option<&kobold_proto::northbound::AskRecord>,
        status: kobold_proto::northbound::LaneStatus,
    ) {
        let idx = if let Some(i) = self.panes.iter().position(|p| p.lane == lane_name) {
            i
        } else {
            self.panes.push(Pane::new(lane_name, branch_name));
            self.panes.len() - 1
        };

        let pane = &mut self.panes[idx];
        pane.branch = branch_name.to_owned();
        pane.transcript.clear();
        for msg in messages {
            let who = match msg.role {
                kobold_proto::northbound::MessageRole::User => Who::User,
                kobold_proto::northbound::MessageRole::Model => Who::Model,
                kobold_proto::northbound::MessageRole::System => Who::System,
            };
            let mut entry = Entry::new(who, msg.text.clone());
            entry.response_id = msg.response_id.clone();
            pane.transcript.push(entry);
        }
        pane.logged = pane.transcript.len();

        pane.panel = None;
        pane.pending_asks.clear();
        if let Some(ask_rec) = active_interrupt {
            let ask = crate::tools::Ask {
                call_id: ask_rec.call_id.clone(),
                question: ask_rec.question.clone(),
                options: ask_rec.options.clone(),
                multiple: ask_rec.multi_select,
            };
            pane.panel = Some(Panel::from(&ask));
            pane.pending_asks.push(ask);
        }

        match status {
            kobold_proto::northbound::LaneStatus::Connecting => {
                pane.link = Link::Connecting;
                pane.outstanding = 0;
            }
            kobold_proto::northbound::LaneStatus::Ready => {
                pane.link = Link::Up;
                pane.outstanding = 0;
            }
            kobold_proto::northbound::LaneStatus::Waiting => {
                pane.link = Link::Up;
                pane.outstanding = 1;
            }
            kobold_proto::northbound::LaneStatus::Gone => {
                pane.link = Link::Gone("connection lost".to_string());
                pane.outstanding = 0;
            }
        }
    }

    /// Fold one AG-UI event into the pane running `lane`.
    ///
    /// The lane comes from the envelope rather than the event, because AG-UI
    /// does not carry one: only `RUN_STARTED` and `RUN_FINISHED` have a
    /// `threadId`, and `RUN_ERROR` has neither a thread nor a message. See
    /// `OutgoingFrame::Event`.
    pub fn apply(&mut self, lane: &str, event: Incoming) -> Effect {
        let Some(idx) = self.panes.iter().position(|p| p.lane == lane) else {
            // A lane with no pane means its pane was closed mid-flight.
            return Effect::None;
        };

        // The specification's own reference server streams OpenAI through the
        // chunk events *exclusively*, so a conforming foreign agent may never
        // send the start/content/end triple at all. Normalised once here
        // rather than handled again in every arm below.
        let event = match event {
            Incoming::TextMessageChunk {
                base,
                message_id,
                delta: Some(delta),
                ..
            } => Incoming::TextMessageContent {
                base,
                message_id: message_id.unwrap_or_default(),
                delta,
            },
            other => other,
        };

        match event {
            Incoming::TextMessageContent { delta, .. } => {
                let voice = self.voice && idx == self.active;
                let pane = &mut self.panes[idx];
                if pane.interrupted {
                    return Effect::None;
                }
                if voice {
                    // First delta of a turn: opening thresholds apply, so the
                    // first clause is short and speech starts sooner.
                    if pane.transcript.last().is_none_or(|e| e.who != Who::Model) {
                        pane.chunker.begin();
                    }
                    let ready = pane.chunker.push(&delta);
                    self.speech.extend(ready);
                }
                let pane = &mut self.panes[idx];
                match pane.open_model.and_then(|i| pane.transcript.get_mut(i)) {
                    Some(e) if e.who == Who::Model => e.text.push_str(&delta),
                    _ => {
                        pane.open_model = Some(pane.transcript.len());
                        pane.transcript.push(Entry::new(Who::Model, delta));
                    }
                }
                pane.scroll = 0;
            }
            Incoming::ToolCallStart {
                tool_call_id,
                tool_call_name,
                ..
            } => {
                self.panes[idx].open_calls.push(PendingCall {
                    id: tool_call_id,
                    name: tool_call_name,
                    arguments: String::new(),
                });
            }
            Incoming::ToolCallArgs {
                tool_call_id,
                delta,
                ..
            } => {
                // Silently dropped when no call is open under that id. An
                // agent that sends arguments for a call it never started is
                // misbehaving, and inventing a call from a fragment with no
                // name is worse than ignoring it -- there would be nothing to
                // dispatch it to.
                if let Some(c) = self.panes[idx]
                    .open_calls
                    .iter_mut()
                    .find(|c| c.id == tool_call_id)
                {
                    c.arguments.push_str(&delta);
                }
            }
            Incoming::ToolCallEnd { tool_call_id, .. } => {
                let pane = &mut self.panes[idx];
                let Some(at) = pane.open_calls.iter().position(|c| c.id == tool_call_id) else {
                    return Effect::None;
                };
                let call = pane.open_calls.remove(at);
                // The machinery, shown only when asked for. What a reader
                // wants by default is the question and their answer, which
                // arrive as ordinary messages once the panel is submitted --
                // see `record_answer`.
                if self.debug {
                    self.panes[idx].transcript.push(Entry::new(
                        Who::System,
                        format!("tool call {} [{}]: {}", call.id, call.name, call.arguments),
                    ));
                }
                return Effect::RunTool(crate::tools::Call {
                    id: call.id,
                    name: call.name,
                    arguments: call.arguments,
                });
            }
            Incoming::RunFinished { run_id, usage, .. } => {
                let voice = self.voice && idx == self.active;
                if let Some(tail) = self.panes[idx].chunker.flush() {
                    if voice {
                        self.speech.push(tail);
                    }
                }
                // An empty run id is no id: the provider's response id is the
                // run id, and a turn that ended without one cannot be chained
                // from. `Some("")` would be a chain to nowhere.
                let response_id = (!run_id.is_empty()).then_some(run_id);
                let pane = &mut self.panes[idx];
                if pane.interrupted {
                    // The server produced a full answer we chose not to show,
                    // so its response id no longer describes our transcript.
                    // Drop the chain; the next turn replays what the user sees.
                    pane.interrupted = false;
                    pane.last_response_id = None;
                } else {
                    pane.last_response_id = response_id.clone();
                    if let Some(e) = pane.open_model.and_then(|i| pane.transcript.get_mut(i)) {
                        e.response_id = response_id;
                    }
                }
                // Recorded even for an interrupted turn: the server generated
                // it and will carry it as context regardless of whether it was
                // shown, so the gauge would otherwise understate what the next
                // turn starts from.
                //
                // Fold B, and it is Kobold's rather than the adapter's because
                // an adapter emits the array and so cannot be the thing that
                // folds it. `None` leaves the gauge alone: a run that reported
                // nothing is not a run that was free.
                if let Some(u) = usage.as_deref().and_then(crate::net::agui::fold_usage) {
                    pane.context = Some(u.total_tokens);
                }
                pane.open_model = None;
                pane.failed = false;
                // One request settled, which is not the same as "this pane
                // is now idle": a tool result sent before this arrived is
                // still outstanding, and that is the case a flag could not
                // express.
                pane.request_settled();
            }
            Incoming::RunError { code, message, .. } => {
                let pane = &mut self.panes[idx];
                pane.request_settled();
                pane.failed = true;
                pane.transcript.push(Entry::new(
                    Who::System,
                    format!("failed: {} {}", code.unwrap_or_else(|| "?".into()), message),
                ));
            }
            // Every event we consume but do not act on yet, and every one the
            // specification grows next. Inert rather than an error: an agent
            // is free to send `TOOL_CALL_RESULT` for a tool it ran itself, or
            // reasoning we do not render until step 4, and neither is a
            // reason to break the turn.
            _ => {}
        }
        Effect::None
    }

    pub fn render(&mut self, buf: &mut Buffer, area: Rect) -> Painted {
        // No borders, no panels. Content sits in a margin and the chrome is one
        // dim line: everything that is not the conversation competes with it.
        // The prompt grows with a multi-line message, capped so it can never
        // crowd out the conversation.
        // The prompt is hidden entirely while this pane has a question open.
        // The panel is where typing goes, and leaving an inert prompt below it
        // invites answering into the wrong place. Per pane, so moving to a
        // split with no question outstanding gets its prompt back.
        let input_rows = if self.pane().panel.is_some() {
            0
        } else {
            self.pane()
                .layout_input(area.width.saturating_sub(4).max(1) as usize)
                .0
                .len()
                .clamp(1, 8)
        } as u16;
        // The suggestion list sits directly above the prompt, so the thing
        // being completed and the completions for it are adjacent. Zero rows
        // when there is nothing to suggest, which is most of the time.
        let menu_rows = self.menu_rows();
        // The panel takes the place the prompt would otherwise have: while it
        // is up, the turn is parked and there is nothing to type into the
        // prompt for, so the two never need rows at the same time.
        let panel_rows = self.panel_rows();
        let [body, menu, panel, input, bar] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(menu_rows),
            Constraint::Length(panel_rows),
            Constraint::Length(input_rows),
            Constraint::Length(1),
        ])
        .areas(area);
        // Margin 1 here, not 2: `render_transcript` adds the second column
        // itself, which leaves a column inside the highlight for padding
        // without shifting text when an entry is selected.
        let body = body.inner(Margin {
            horizontal: 1,
            vertical: 1,
        });
        let input = input.inner(Margin {
            horizontal: 2,
            vertical: 0,
        });
        let menu = menu.inner(Margin {
            horizontal: 2,
            vertical: 0,
        });
        let panel = panel.inner(Margin {
            horizontal: 2,
            vertical: 0,
        });
        let bar = bar.inner(Margin {
            horizontal: 2,
            vertical: 0,
        });

        // Panes share the body evenly, separated by a dim rule so the split is
        // visible without a border around each column.
        // `split`, not `areas`: the pane count is decided at runtime by forks,
        // so the const-generic form cannot express it.
        let n = self.panes.len();
        let cols = Layout::horizontal(vec![Constraint::Ratio(1, n as u32); n]).split(body);
        // The rule between panes eats two columns from every pane but the
        // first, so the text area is resolved once here and reused for both
        // the layout pass and the draw.
        let areas: Vec<Rect> = cols
            .iter()
            .enumerate()
            .map(|(i, col)| {
                if i == 0 {
                    *col
                } else {
                    Rect {
                        x: col.x + 2,
                        width: col.width.saturating_sub(2),
                        ..*col
                    }
                }
            })
            .collect();

        // Lay entries out before drawing anything. This is where the memoised
        // work lands: only entries whose text or width changed are rebuilt, so
        // a streaming delta re-parses one entry rather than the transcript.
        //
        // Tallied locally and folded in once at the end: an atomic per entry
        // is measurable on a long transcript, and an instrument that slows
        // what it measures is a bad one. See `layout::Tally`.
        let mut tally = crate::layout::Tally::default();
        for (i, area) in areas.iter().enumerate() {
            let width = transcript_width(*area);
            for entry in self.panes[i].transcript.iter_mut() {
                tally.add(entry.refresh(width, self.code_bg));
            }
        }
        tally.flush();

        let mut scrolled = None;
        for (i, col) in cols.iter().enumerate() {
            if i > 0 {
                let rule = Style::default().fg(Color::Indexed(237));
                for y in col.top()..col.bottom() {
                    buf[(col.x, y)].set_symbol("\u{2502}").set_style(rule);
                }
            }
            let offset = self.render_transcript(buf, areas[i], i);

            // A scroll hint only makes sense when the moving rows span the
            // full width. With a split the band would take the neighbouring
            // pane with it, so panes are repainted the ordinary way.
            let single = self.panes.len() == 1;
            let pane = &mut self.panes[i];
            let moved = match pane.last_paint {
                // Only when the geometry is the one we measured against:
                // after a resize the previous offset describes a different
                // layout.
                Some((was, prev)) if single && was == areas[i] => {
                    i32::from(offset) - i32::from(prev)
                }
                _ => 0,
            };
            pane.last_paint = Some((areas[i], offset));
            if moved != 0 && moved.unsigned_abs() < u32::from(areas[i].height) {
                scrolled = Some((areas[i].top()..areas[i].bottom(), moved));
            }
        }
        if menu_rows > 0 {
            self.render_menu(buf, menu);
        }
        // Exactly one of these is showing: the prompt is hidden while this
        // pane has a question open, so the caret cannot be in two places.
        let panel_caret = (panel_rows > 0)
            .then(|| self.render_panel(buf, panel))
            .flatten();
        let input_caret = (self.pane().panel.is_none())
            .then(|| self.render_input(buf, input))
            .flatten();
        self.render_bar(buf, bar);
        Painted {
            cursor: panel_caret.or(input_caret),
            scroll: scrolled,
        }
    }

    /// Rows the suggestion list needs, zero when there is nothing to suggest.
    ///
    /// Capped so the list can never push the conversation off screen. Three
    /// commands cannot reach that today, which is exactly when to write the
    /// bound rather than rely on the count staying small.
    fn menu_rows(&self) -> u16 {
        const MAX: usize = 8;
        crate::complete::matches(&self.pane().input).len().min(MAX) as u16
    }

    /// Rows the bottom panel needs: one per row plus a blank one so the
    /// question has room above the options, zero when it is closed.
    fn panel_rows(&self) -> u16 {
        match &self.pane().panel {
            // The navigable rows, plus a heading line for each question that
            // has options. A `Text` field's prompt is inline, so it adds none.
            Some(p) => (p.row_count() + p.headings()) as u16,
            None => 0,
        }
    }

    /// The slash-command suggestions, newest idea first: the name in normal
    /// weight, what it does dim beside it, and the highlighted row tinted the
    /// full width so it reads as a band rather than stopping at the text --
    /// the same visual language as a browsed transcript entry.
    fn render_menu(&self, buf: &mut Buffer, area: Rect) {
        const BG: Color = Color::Indexed(238);
        let dim = Style::default().fg(Color::DarkGray);
        let selected = self.menu_index();
        for (row, cmd) in crate::complete::matches(&self.pane().input)
            .iter()
            .take(area.height as usize)
            .enumerate()
        {
            let y = area.y + row as u16;
            let chosen = row == selected;
            let tint = Tint {
                bg: chosen.then_some(BG),
                ..Tint::default()
            };
            let name = if chosen {
                Style::default().fg(Color::Green)
            } else {
                Style::default()
            };
            // Descriptions share a column, so the eye runs down them instead
            // of stepping in and out with each name's length.
            let pad = crate::complete::COMMANDS
                .iter()
                .map(|c| c.name.len())
                .max()
                .unwrap_or(0);
            let line = Line::from(vec![
                Span::styled(format!("/{:<pad$}", cmd.name), name),
                Span::styled(format!("   {}", cmd.help), dim),
            ]);
            let x = blit(buf, area.x, y, area.right(), &line, tint);
            if chosen {
                fill(buf, x, y, area.right(), Style::default().bg(BG));
            }
        }
    }

    /// The bottom form panel: a heading (the field carrying the question --
    /// see `From<&Ask>`) over its rows, radio/checkbox markers on `Choice`
    /// rows and the live-typed value on a `Text` row, the highlighted row
    /// tinted full width the same way as the slash-command menu.
    /// Returns where the terminal caret belongs: inside the focused text field,
    /// or `None` on a choice row, where there is nothing to type into.
    fn render_panel(&self, buf: &mut Buffer, area: Rect) -> Option<Position> {
        const BG: Color = Color::Indexed(238);
        let Some(panel) = &self.pane().panel else {
            return None;
        };
        let dim = Style::default().fg(Color::DarkGray);
        let mut caret = None;
        let mut y = area.y;
        let mut row = 0usize;

        // Walked by field rather than by row, because a panel can be carrying
        // more than one question and each needs its own heading above its
        // options. Only `Choice` gets one: a `Text` field's prompt is its
        // inline label, so a heading would say the same thing twice.
        for (fi, field) in panel.fields.iter().enumerate() {
            if y >= area.bottom() {
                break;
            }
            match field {
                Field::Choice {
                    prompt,
                    options,
                    multi,
                } => {
                    if !prompt.is_empty() {
                        let line = Line::from(Span::raw(prompt.clone()));
                        blit(buf, area.x, y, area.right(), &line, Tint::default());
                        y += 1;
                    }
                    for (oi, option) in options.iter().enumerate() {
                        if y >= area.bottom() {
                            break;
                        }
                        let chosen = row == panel.row;
                        let tint = Tint {
                            bg: chosen.then_some(BG),
                            ..Tint::default()
                        };
                        let marker = match (multi, panel.chosen(fi, oi)) {
                            (true, true) => "[x] ",
                            (true, false) => "[ ] ",
                            (false, true) => "(\u{2022}) ",
                            (false, false) => "( ) ",
                        };
                        // The digit that selects this option, per `Panel::char`
                        // -- rendered because an unlabelled shortcut is one
                        // nobody finds. `tools::MAX_OPTIONS` keeps this a
                        // single character.
                        let numbered = format!("{}) {marker}", oi + 1);
                        let line = Line::from(vec![Span::raw(numbered), Span::raw(option.clone())]);
                        let x = blit(buf, area.x, y, area.right(), &line, tint);
                        if chosen {
                            fill(buf, x, y, area.right(), Style::default().bg(BG));
                        }
                        y += 1;
                        row += 1;
                    }
                }
                Field::Text { prompt } => {
                    let chosen = row == panel.row;
                    let tint = Tint {
                        bg: chosen.then_some(BG),
                        ..Tint::default()
                    };
                    let label = format!("{prompt}: ");
                    let text = panel.text_of(fi).to_owned();
                    if chosen {
                        // A real caret at the typing position, rather than a
                        // block after the text -- which was the only option
                        // when the field could not have one, and would now
                        // point at the wrong place.
                        let before: String =
                            text.chars().take(panel.text_caret().unwrap_or(0)).collect();
                        let x = area.x
                            + crate::md::width_of(&label) as u16
                            + crate::md::width_of(&before) as u16;
                        caret = Some(Position::new(x.min(area.right().saturating_sub(1)), y));
                    }
                    let line = Line::from(vec![Span::styled(label, dim), Span::raw(text)]);
                    let x = blit(buf, area.x, y, area.right(), &line, tint);
                    if chosen {
                        fill(buf, x, y, area.right(), Style::default().bg(BG));
                    }
                    y += 1;
                    row += 1;
                }
            }
        }
        caret
    }

    fn render_bar(&self, buf: &mut Buffer, area: Rect) {
        let dim = Style::default().fg(Color::DarkGray);
        let accent = match self.mode {
            Mode::Send => Style::default().fg(Color::Green),
            Mode::Chat => Style::default().fg(Color::Magenta),
        };
        if self.notice_live() {
            let (text, _) = self.notice.as_ref().expect("checked live");
            let line = Line::from(vec![
                Span::styled(self.mode.label(), accent),
                Span::styled(format!("   {text}"), Style::default().fg(Color::Blue)),
            ]);
            blit(buf, area.x, area.y, area.right(), &line, Tint::default());
            return;
        }
        let hint = if self.quit_armed {
            "press ^D again to quit".to_owned()
        } else if self.panel_open() {
            "\u{2191}\u{2193} move   space toggle   enter submit   esc decline".to_owned()
        } else {
            match self.mode {
                // While the list is up the arrows drive it rather than
                // history, and the bar exists precisely so the live bindings
                // are never a guess.
                Mode::Send if self.menu_open() => {
                    "\u{2191}\u{2193} choose   \u{21e5} complete".to_owned()
                }
                // The hint above the prompt is gone by now, having been
                // replaced by a real argument, so this is what says the key
                // still does something.
                Mode::Send if self.args_open() => "\u{21e5} arguments".to_owned(),
                Mode::Send if !self.pane().queue.is_empty() => {
                    format!("{} queued   esc unqueue", self.pane().queue.len())
                }
                Mode::Send if self.pane().status() == Status::Waiting => {
                    "esc esc interrupt".to_owned()
                }
                Mode::Send if self.panes.len() > 1 => {
                    format!(
                        "↑↓ history   ⇧↑ browse   ⇧←→ pane {}/{}   ^W close",
                        self.active + 1,
                        self.panes.len()
                    )
                }
                Mode::Send => "↑↓ history   ⇧↑ browse   ^D^D quit".to_owned(),
                Mode::Chat if crate::osc::clipboard_supported() => {
                    "⇧↑↓ select   y yank   r rewind   f fork   esc back".to_owned()
                }
                Mode::Chat => "⇧↑↓ select   r rewind   f fork   esc back".to_owned(),
            }
        };
        let line = Line::from(vec![
            Span::styled(self.mode.label(), accent),
            Span::styled(
                if self.voice { "  voice" } else { "" },
                Style::default().fg(Color::Magenta),
            ),
            Span::styled(format!("   {hint}"), dim),
        ]);
        let used = blit(buf, area.x, area.y, area.right(), &line, Tint::default());
        self.render_status(buf, area, used);
    }

    /// What is running and how full its context is, laid out from the right
    /// edge back.
    ///
    /// Right-aligned because it changes on its own schedule rather than with
    /// the keys, so anchoring it to the edge keeps it from shuffling sideways
    /// every time the hint on the left changes length. Dropped entirely rather
    /// than overlapped when the hint is long enough to reach it: the hint says
    /// what the next keypress does and wins.
    fn render_status(&self, buf: &mut Buffer, area: Rect, used: u16) {
        // Shed the model before the gauge. The model is the same all session
        // and is one `/help` away; the gauge is the only thing on screen that
        // changes as the context fills, so it is the part worth the columns.
        for with_model in [true, false] {
            let Some(right) = self.status_line(with_model) else {
                continue;
            };
            let width = crate::md::width_of(&right.to_string()) as u16;
            // Two columns of clearance, so the halves never look joined.
            let x = area.right().saturating_sub(width);
            if x >= used + 2 {
                blit(buf, x, area.y, area.right(), &right, Tint::default());
                return;
            }
        }
    }

    /// `model \u{b7} effort   \u{2588}\u{2588}\u{2591}\u{2591} 47%`, or as
    /// much of it as there is to say.
    fn status_line(&self, with_model: bool) -> Option<Line<'static>> {
        let dim = Style::default().fg(Color::DarkGray);
        let mut spans: Vec<Span<'static>> = Vec::new();
        if with_model && !self.model.is_empty() {
            let label = if !self.harness.is_empty() {
                format!("{} \u{b7} {}", self.harness, self.model)
            } else {
                self.model.clone()
            };
            spans.push(Span::styled(label, dim));
            if !self.effort.is_empty() {
                spans.push(Span::styled(format!(" \u{b7} {}", self.effort), dim));
            }
        }

        if let Some(tokens) = self.pane().context {
            if !spans.is_empty() {
                spans.push(Span::styled("   ", dim));
            }
            match self.context_window {
                // No limit configured, so report the count and claim nothing
                // about how much room is left.
                0 => spans.push(Span::styled(thousands(tokens), dim)),
                limit => {
                    const CELLS: usize = 8;
                    let frac = (tokens as f64 / limit as f64).clamp(0.0, 1.0);
                    let full = (frac * CELLS as f64).round() as usize;
                    // Coloured only once it is worth reacting to: a gauge that
                    // is always coloured says nothing by being coloured.
                    let tone = match frac {
                        f if f >= 0.90 => Style::default().fg(Color::Red),
                        f if f >= 0.75 => Style::default().fg(Color::Yellow),
                        _ => dim,
                    };
                    spans.push(Span::styled("\u{2588}".repeat(full), tone));
                    spans.push(Span::styled("\u{2591}".repeat(CELLS - full), dim));
                    spans.push(Span::styled(format!(" {:.0}%", frac * 100.0), tone));
                }
            }
        }
        (!spans.is_empty()).then(|| Line::from(spans))
    }

    /// Returns the row offset it scrolled to, so the caller can tell whether
    /// the viewport merely slid.
    fn render_transcript(&mut self, buf: &mut Buffer, area: Rect, pane_idx: usize) -> u16 {
        let dim = Style::default().fg(Color::DarkGray);
        let width = transcript_width(area);

        // Taken out and put back so the same allocation is reused every
        // frame, and so the borrow checker will let pass two read the
        // transcript while this is in hand.
        let mut ranges = std::mem::take(&mut self.panes[pane_idx].ranges);
        ranges.clear();
        let pane = &self.panes[pane_idx];

        // Pass one: where every entry's rows go -- one span per entry, not
        // one slot per row. A prefix sum over lengths that are already laid
        // out and memoised, so this is O(entries) and allocates nothing.
        //
        // It used to build a `Vec<Slot>` covering every row of every entry,
        // which made placing thirty rows cost thirty-eight thousand at two
        // hundred turns. The spans carry the same information: pass two
        // binary-searches them for the entry a row belongs to.
        let mut rows = 0usize;
        let mut prev: Option<Who> = None;
        for entry in pane.transcript.iter() {
            // Blank line between turns, but not between consecutive system
            // notes -- those read as one block.
            let joined = prev == Some(Who::System) && entry.who == Who::System;
            if rows != 0 && !joined {
                rows += 1;
            }
            prev = Some(entry.who);
            let start = rows;
            rows += entry.lines.len();
            ranges.push((start, rows));
        }
        let entry_rows = rows;

        // Rows that belong to no entry, and so cannot be memoised: they
        // change every frame or every keystroke. Bounded by the queue rather
        // than by the transcript, so these stay materialised.
        let mut extra: Vec<Line<'static>> = Vec::new();
        let mut tail: Vec<Slot> = Vec::new();

        // The spinner lives where the answer will land, not on the prompt, and
        // stays until the turn completes -- so it sits below the streamed text
        // once deltas start arriving.
        if pane.status() == Status::Waiting {
            if entry_rows != 0 {
                tail.push(Slot::Blank);
            }
            extra.push(Line::from(Span::styled(
                SPINNER[self.spinner % SPINNER.len()],
                Style::default().fg(Color::Yellow),
            )));
            tail.push(Slot::Extra(extra.len() as u32 - 1));
        }

        // Queued messages are shown where they will land, dimmed, so it is
        // obvious what is about to be sent and in what order.
        for q in &pane.queue {
            tail.push(Slot::Blank);
            for chunk in wrap(q, width.saturating_sub(2)) {
                extra.push(Line::from(vec![
                    Span::styled("\u{203a} ", dim),
                    Span::styled(chunk, dim),
                ]));
                tail.push(Slot::Extra(extra.len() as u32 - 1));
            }
        }

        // A real blank row above and below the browsed entry rather than
        // borrowing the separator, so the first entry in a transcript gets the
        // same padding as any other. Not inserted: two inserts would shift
        // every row after them, and `unblank` expresses the same result as a
        // constant offset.
        let mut span: Option<(usize, usize)> = None;
        let mut focus: Option<(usize, usize)> = None;
        if let Some(sel) = pane.selected.filter(|_| pane_idx == self.active) {
            if let Some(&(a, b)) = ranges.get(sel) {
                span = Some((a, b));
                focus = Some((a, b + 2));
            }
        }

        let total = (entry_rows + tail.len() + usize::from(span.is_some()) * 2) as u16;
        let height = area.height;
        let offset = match focus {
            // Keep the selection on screen, preferring to show its start.
            Some((a, b)) => {
                let a = a as u16;
                let b = b as u16;
                if b.saturating_sub(a) >= height {
                    a
                } else {
                    a.min(total.saturating_sub(height))
                        .max(b.saturating_sub(height))
                }
            }
            None => total.saturating_sub(height).saturating_sub(pane.scroll),
        };

        // Pass two: write only the rows the viewport shows, straight into the
        // buffer. Everything here -- the pad column, dimming, the selection
        // tint -- used to run over the whole transcript to display one
        // screenful, and used to allocate a `Line` per row to do it.
        const BG: Color = Color::Indexed(238);
        let hl = Style::default().bg(BG);
        let right = area.right();
        let drawn = (total.saturating_sub(offset)).min(height);
        for row in 0..drawn {
            let i = offset as usize + row as usize;
            let y = area.y + row;
            // Highlight the browsed entry by tinting its full width, the same
            // visual language as a code slab so there is one idea of "block".
            let selected = focus.is_some_and(|(top, bottom)| i >= top && i < bottom);
            let tint = Tint {
                dim: pane_idx != self.active,
                bg: selected.then_some(BG),
                bold: focus.is_some_and(|(top, bottom)| i > top && i < bottom - 1),
            };

            // Every row gets a leading pad column. Constant, so selecting an
            // entry tints that column instead of nudging the text sideways.
            buf[(area.x, y)]
                .set_char(' ')
                .set_style(tint.apply(Style::default()));
            let mut x = area.x + 1;

            // Which row of the underlying layout this display row shows: the
            // same answer the old `Vec<Slot>` held at index `i`, computed
            // instead of stored.
            let under = match span {
                Some((a, b)) => unblank(i, a, b),
                None => Some(i),
            };
            x = match under {
                None => x,
                Some(u) if u < entry_rows => match entry_at(&ranges, u) {
                    Some((e, li)) => blit(buf, x, y, right, &pane.transcript[e].lines[li], tint),
                    // A separator between two entries, which belongs to
                    // neither and so appears in no span.
                    None => x,
                },
                Some(u) => match tail.get(u - entry_rows) {
                    Some(Slot::Extra(k)) => blit(buf, x, y, right, &extra[*k as usize], tint),
                    _ => x,
                },
            };

            if selected {
                // Tinted to the full width, and without the bold or the fg
                // lift: this is background, not text.
                fill(buf, x, y, right, hl);
            }
        }

        // Rows placed, which is now the viewport rather than the transcript,
        // and entries visited, which is still one fold over all of them.
        crate::layout::pass(drawn as u64, pane.transcript.len() as u64);
        self.panes[pane_idx].ranges = ranges;
        offset
    }

    fn render_input(&self, buf: &mut Buffer, area: Rect) -> Option<Position> {
        let dim = Style::default().fg(Color::DarkGray);
        let pane = self.pane();

        let (mark, mark_style) = match pane.status() {
            Status::Gone => ("✕", Style::default().fg(Color::Red)),
            Status::Connecting => ("·", dim),
            // Waiting looks the same as ready. Activity is reported once, by
            // the spinner sitting where the answer will land.
            Status::Ready | Status::Waiting => ("›", dim),
        };

        // What the named command takes, trailing the text in grey. Only ever
        // on the last row, which is where the line ends and where the caret is
        // when someone has just finished typing a name.
        let hint = crate::complete::hint(&pane.input, self.voice);
        let (rows, (cy, cx)) = pane.layout_input(area.width.saturating_sub(2) as usize);
        // Show the window containing the caret when the input outgrows the box.
        let top = cy.saturating_sub(area.height.saturating_sub(1) as usize);

        let right = area.right();
        for (n, (i, row)) in rows
            .iter()
            .enumerate()
            .skip(top)
            .take(area.height as usize)
            .enumerate()
        {
            let head = if i == 0 { mark } else { " " };
            let style = if i == 0 { mark_style } else { dim };
            let mut spans = vec![
                Span::styled(head, style),
                Span::raw(" "),
                Span::raw(row.as_str()),
            ];
            if i + 1 == rows.len() {
                if let Some(args) = &hint {
                    spans.push(Span::styled(format!("   {args}"), dim));
                }
            }
            // Why the connection went away, next to the marker that says so.
            if let (0, Some(why)) = (n, pane.gone_reason()) {
                spans.push(Span::styled(format!("  {why}"), dim));
            }
            let line = Line::from(spans);
            blit(
                buf,
                area.x,
                area.y + n as u16,
                right,
                &line,
                Tint::default(),
            );
        }

        if pane.status() == Status::Gone {
            return None;
        }
        let x = area.x + 2 + cx as u16;
        let y = area.y + (cy - top) as u16;
        Some(Position::new(
            x.min(area.right().saturating_sub(1)),
            y.min(area.bottom().saturating_sub(1)),
        ))
    }
}

#[cfg(test)]
mod tests {

    /// A finished run, which is what `Update::Completed` used to be. The
    /// lane travels in the envelope now, so it is `apply`'s first argument
    /// rather than a field of the event.
    fn finished(response_id: Option<&str>) -> Incoming {
        Incoming::RunFinished {
            base: Default::default(),
            thread_id: String::new(),
            run_id: response_id.unwrap_or_default().to_owned(),
            usage: None,
            outcome: None,
            result: None,
        }
    }

    /// One text delta, the shape almost every assertion here needs.
    fn content(message_id: &str, delta: &str) -> Incoming {
        Incoming::TextMessageContent {
            base: Default::default(),
            message_id: message_id.to_owned(),
            delta: delta.to_owned(),
        }
    }

    fn finished_costing(total: u32) -> Incoming {
        Incoming::RunFinished {
            base: Default::default(),
            thread_id: String::new(),
            run_id: String::new(),
            usage: Some(vec![crate::net::agui::TokenUsage {
                total_tokens: Some(total),
                ..Default::default()
            }]),
            outcome: None,
            result: None,
        }
    }

    fn text(delta: &str) -> Incoming {
        Incoming::TextMessageContent {
            base: Default::default(),
            message_id: "m".to_owned(),
            delta: delta.to_owned(),
        }
    }

    /// The three events one tool call arrives as. Returned together because
    /// no single one of them is a call: `apply` assembles them and only the
    /// last returns an `Effect`.
    fn tool_call(call_id: &str, name: &str, arguments: &str) -> Vec<Incoming> {
        vec![
            Incoming::ToolCallStart {
                base: Default::default(),
                tool_call_id: call_id.to_owned(),
                tool_call_name: name.to_owned(),
                parent_message_id: None,
            },
            Incoming::ToolCallArgs {
                base: Default::default(),
                tool_call_id: call_id.to_owned(),
                delta: arguments.to_owned(),
            },
            Incoming::ToolCallEnd {
                base: Default::default(),
                tool_call_id: call_id.to_owned(),
            },
        ]
    }
    use super::*;
    /// The visible screen as text, which is what these tests actually care
    /// about: the layout cache is an optimisation and must be invisible.
    fn frame(app: &mut App, cols: u16, rows: u16) -> String {
        let area = Rect::new(0, 0, cols, rows);
        let mut buffer = Buffer::empty(area);
        app.render(&mut buffer, area);
        let buf = &buffer;
        (0..rows)
            .map(|y| (0..cols).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The rows the transcript highlighted, as text, with their y positions.
    ///
    /// Reads the tint out of the buffer rather than trusting any internal
    /// index, because the whole point is to check the index against what
    /// actually landed on screen.
    fn highlighted(app: &mut App, cols: u16, rows: u16) -> Vec<(u16, String)> {
        let area = Rect::new(0, 0, cols, rows);
        let mut buffer = Buffer::empty(area);
        app.render(&mut buffer, area);
        let buf = &buffer;
        (0..rows)
            .filter(|&y| (0..cols).any(|x| buf[(x, y)].bg == Color::Indexed(238)))
            .map(|y| {
                let text: String = (0..cols).map(|x| buf[(x, y)].symbol()).collect();
                (y, text.trim().to_owned())
            })
            .collect()
    }

    /// A transcript whose entries name themselves, so a row can be traced
    /// back to the entry it came from.
    fn numbered(n: usize) -> App {
        let mut app = App::new("main", "b0");
        for i in 0..n {
            app.push(Who::User, format!("question-{i:03}"));
            app.push(Who::Model, format!("answer-{i:03}"));
        }
        app.apply_transport(Transport::Connected);
        app
    }

    /// The property that makes a row-mapping bug dangerous rather than
    /// cosmetic: what the screen highlights must be the entry `pane.selected`
    /// names, because that index is what `rewind` and `fork` act on.
    ///
    /// A mapping off by one shows entry N highlighted while a rewind cuts at
    /// N±1 -- silent data loss, reported by nothing. Written before the
    /// placement pass was rewritten, so it passes against the old code and
    /// has to keep passing against the new.
    #[test]
    fn the_highlighted_rows_belong_to_the_entry_selected_is_pointing_at() {
        // Several selections, and several scroll offsets for each, because
        // the mapping is offset arithmetic and an error in it need not show
        // at every position.
        for pick in [0usize, 1, 7, 18, 19] {
            for scroll in [0u16, 3, 11] {
                let mut app = numbered(10);
                let entries = app.pane().transcript.len();
                assert_eq!(entries, 20);
                app.mode = Mode::Chat;
                app.pane_mut().selected = Some(pick);
                app.pane_mut().scroll = scroll;

                let want = app.pane().transcript[pick].text.clone();
                let rows = highlighted(&mut app, 40, 14);
                let texts: Vec<&str> = rows
                    .iter()
                    .map(|(_, t)| t.as_str())
                    .filter(|t| !t.is_empty())
                    .collect();

                // Nothing highlighted at all is a legitimate outcome only if
                // the selection is scrolled off screen -- but with a
                // selection set, the viewport is pinned to it, so it must be
                // visible. This is the counter-assertion: "no rows tinted"
                // would otherwise satisfy every check below.
                assert!(
                    !texts.is_empty(),
                    "selection {pick} at scroll {scroll} was highlighted nowhere on screen"
                );
                // Contains rather than equals: a user row carries a leading
                // marker, so the entry's text is a substring of the row.
                for t in &texts {
                    assert!(
                        t.contains(&want),
                        "selection {pick} at scroll {scroll}: highlighted {t:?}, but \
                         pane.selected names {want:?} -- a rewind here would cut in the \
                         wrong place"
                    );
                }

                // The tinted rows are one contiguous run: the entry's own
                // rows plus a blank bracketing each end. A gap would mean two
                // different entries were being tinted.
                let ys: Vec<u16> = rows.iter().map(|(y, _)| *y).collect();
                for pair in ys.windows(2) {
                    assert_eq!(
                        pair[1],
                        pair[0] + 1,
                        "selection {pick} at scroll {scroll}: highlight is not contiguous, \
                         rows {ys:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn nothing_is_highlighted_when_no_entry_is_selected() {
        // The other half of the pair above: with no selection there must be
        // no tint at all, or "highlighted the right thing" could be satisfied
        // by highlighting everything.
        let mut app = numbered(10);
        app.pane_mut().selected = None;
        assert!(
            highlighted(&mut app, 40, 14).is_empty(),
            "rows were tinted with nothing selected"
        );
    }

    #[test]
    fn entry_at_agrees_with_actually_expanding_the_spans() {
        // Checked against a reference that materialises the per-row vector
        // this replaced -- which is the exact thing the change deleted, so
        // the old representation gets to be the oracle for the new one.
        // Includes an empty entry, because two spans can then share a start
        // and only taking the last one steps over it correctly.
        for lens in [
            vec![1usize],
            vec![3],
            vec![1, 1],
            vec![2, 3],
            vec![0, 2],
            vec![2, 0, 3],
            vec![1, 0, 0, 2],
            vec![4, 1, 2],
        ] {
            let mut ranges = Vec::new();
            let mut reference: Vec<Option<(usize, usize)>> = Vec::new();
            let mut rows = 0usize;
            for (e, &n) in lens.iter().enumerate() {
                if rows != 0 {
                    reference.push(None); // the separator between turns
                    rows += 1;
                }
                let start = rows;
                for li in 0..n {
                    reference.push(Some((e, li)));
                }
                rows += n;
                ranges.push((start, rows));
            }
            for (r, want) in reference.iter().enumerate() {
                assert_eq!(entry_at(&ranges, r), *want, "row {r} of {lens:?}");
            }
            // Past the end is nobody's row, which is what stops the tail
            // being attributed to the last entry.
            assert_eq!(entry_at(&ranges, rows), None, "past the end of {lens:?}");
        }
    }

    #[test]
    fn entry_at_finds_nothing_in_an_empty_transcript() {
        assert_eq!(entry_at(&[], 0), None);
        assert_eq!(entry_at(&[], 7), None);
    }

    #[test]
    fn unblank_agrees_with_actually_inserting_the_two_blanks() {
        // Checked against a reference that does the insertion the slow way,
        // over every span and every row, rather than against hand-written
        // expectations -- the arithmetic is exactly the kind that looks right
        // and is off by one, so the test should not be derived from the same
        // reasoning as the code.
        for n in 0..8usize {
            for a in 0..=n {
                for b in a..=n {
                    let mut reference: Vec<Option<usize>> = (0..n).map(Some).collect();
                    reference.insert(b, None);
                    reference.insert(a, None);
                    for (r, want) in reference.iter().enumerate() {
                        assert_eq!(
                            unblank(r, a, b),
                            *want,
                            "row {r} of {n} with the entry at [{a}, {b})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn unblank_puts_a_blank_at_each_end_of_the_span_and_nowhere_else() {
        // The property stated directly, so a reference that happened to be
        // wrong in the same way could not carry both tests.
        let (a, b) = (3usize, 6usize);
        let blanks: Vec<usize> = (0..12).filter(|&r| unblank(r, a, b).is_none()).collect();
        assert_eq!(
            blanks,
            vec![a, b + 1],
            "exactly two blanks, bracketing the span"
        );
        // And the rows either side are the ones the span actually covers.
        assert_eq!(
            unblank(a - 1, a, b),
            Some(a - 1),
            "the row above is untouched"
        );
        assert_eq!(
            unblank(a + 1, a, b),
            Some(a),
            "the span starts just below the blank"
        );
        assert_eq!(
            unblank(b, a, b),
            Some(b - 1),
            "and ends just above the second"
        );
        assert_eq!(
            unblank(b + 2, a, b),
            Some(b),
            "after which everything shifts by two"
        );
    }

    /// Rows of the transcript viewport, trimmed, top to bottom.
    fn rows_of(app: &mut App, cols: u16, rows: u16) -> Vec<String> {
        frame(app, cols, rows)
            .lines()
            .map(|l| l.trim().to_owned())
            .collect()
    }

    #[test]
    fn a_waiting_pane_shows_the_spinner_and_a_ready_one_does_not() {
        // The spinner sits below the transcript, where the answer will land.
        // It is the first of the rows that belong to no entry, so it is also
        // the simplest check that the tail is placed at all.
        let mut app = numbered(2);
        app.pane_mut().sent_request();
        let seen = frame(&mut app, 40, 14);
        assert!(
            SPINNER.iter().any(|s| seen.contains(s)),
            "no spinner while waiting:\n{seen}"
        );

        // An empty transcript, which is the state a session opens in: the
        // spinner is then the very first row, with no separator above it
        // because there is nothing to separate it from. Worth its own case
        // because it is the one row that is neither an entry's nor behind a
        // blank, so it is where the boundary between the two regions is
        // actually observable.
        let mut app = App::new("main", "b0");
        app.pane_mut().sent_request();
        let rows = rows_of(&mut app, 40, 14);
        let at = rows
            .iter()
            .position(|r| SPINNER.iter().any(|s| r.contains(s)));
        let at = at.unwrap_or_else(|| panic!("no spinner on an empty waiting pane:\n{rows:?}"));

        // Where the first row of the transcript actually lands, measured
        // rather than assumed -- the area carries a margin, so a hardcoded
        // index would be pinning the layout instead of the spinner. A single
        // idle entry occupies exactly that row.
        let mut one = App::new("main", "b0");
        one.apply_transport(Transport::Connected);
        one.push(Who::Model, "only-entry");
        let first_row = rows_of(&mut one, 40, 14)
            .iter()
            .position(|r| r.contains("only-entry"))
            .expect("the one entry");
        assert_eq!(
            at, first_row,
            "with nothing above it the spinner belongs on the transcript's first row, \
             not pushed down by a separator it does not need:\n{rows:?}"
        );

        // The counter-assertion: a spinner drawn unconditionally would pass
        // the check above just as well.
        let mut app = numbered(2);
        app.apply_transport(Transport::Connected);
        let seen = frame(&mut app, 40, 14);
        assert!(
            !SPINNER.iter().any(|s| seen.contains(s)),
            "spinner still showing when idle:\n{seen}"
        );
    }

    #[test]
    fn queued_messages_are_shown_below_the_transcript_in_order() {
        // Queued turns are drawn where they will land so it is obvious what
        // is about to be sent and in what order -- which means both that they
        // appear and that they appear after the transcript, not before it.
        let mut app = numbered(1);
        app.pane_mut().queue.push("queued-first".to_owned());
        app.pane_mut().queue.push("queued-second".to_owned());
        let rows = rows_of(&mut app, 40, 14);

        let at = |needle: &str| rows.iter().position(|r| r.contains(needle));
        let (answer, first, second) = (at("answer-000"), at("queued-first"), at("queued-second"));
        assert!(
            first.is_some() && second.is_some(),
            "queued messages missing from:\n{rows:?}"
        );
        assert!(answer < first, "the queue must sit below the transcript");
        assert!(first < second, "and in the order it will be sent");

        // Empty queue, nothing drawn: otherwise "shown in order" could be
        // satisfied by drawing something unconditionally.
        let mut app = numbered(1);
        let rows = rows_of(&mut app, 40, 14);
        assert!(
            !rows.iter().any(|r| r.contains("queued-")),
            "queue rows drawn with an empty queue"
        );
    }

    #[test]
    fn a_blank_separates_turns_but_not_consecutive_system_notes() {
        // Two system notes read as one block, so the separator is suppressed
        // between them and only between them.
        let mut app = App::new("main", "b0");
        app.apply_transport(Transport::Connected);
        app.push(Who::System, "note-one");
        app.push(Who::System, "note-two");
        let rows = rows_of(&mut app, 40, 14);
        let a = rows
            .iter()
            .position(|r| r.contains("note-one"))
            .expect("note-one");
        let b = rows
            .iter()
            .position(|r| r.contains("note-two"))
            .expect("note-two");
        assert_eq!(
            b,
            a + 1,
            "consecutive system notes must not be separated:\n{rows:?}"
        );

        // A different speaker either side, and the blank comes back. This is
        // the half that fails if the separator stops being emitted at all.
        let mut app = App::new("main", "b0");
        app.apply_transport(Transport::Connected);
        app.push(Who::User, "spoken");
        app.push(Who::System, "note-one");
        let rows = rows_of(&mut app, 40, 14);
        let a = rows
            .iter()
            .position(|r| r.contains("spoken"))
            .expect("spoken");
        let b = rows
            .iter()
            .position(|r| r.contains("note-one"))
            .expect("note-one");
        assert_eq!(b, a + 2, "a turn boundary needs its blank row:\n{rows:?}");
    }

    #[test]
    fn selecting_an_entry_adds_the_two_blank_rows_to_the_total() {
        // The selection brackets its entry with real blank rows, which makes
        // the transcript two rows taller. Observable as the content shifting
        // up by one when the viewport is already full: the bracket above the
        // selection pushes everything down, and the scroll compensates.
        let mut app = numbered(6);
        app.mode = Mode::Chat;
        let plain = rows_of(&mut app, 40, 14);
        app.pane_mut().selected = Some(11);
        let picked = rows_of(&mut app, 40, 14);
        assert_ne!(plain, picked, "selecting must change the rows on screen");

        // The bracket rows are blank and sit either side of the entry.
        let hl = highlighted(&mut app, 40, 14);
        assert!(
            hl.len() >= 3,
            "expected a blank, the entry, and a blank: {hl:?}"
        );
        assert_eq!(
            hl.first().map(|(_, t)| t.as_str()),
            Some(""),
            "top bracket is blank"
        );
        assert_eq!(
            hl.last().map(|(_, t)| t.as_str()),
            Some(""),
            "bottom bracket is blank"
        );
    }

    const TEXT: &str = "Congestion control, in **one** line with `code` and a tail long \
                        enough that it has to wrap somewhere.";

    #[test]
    fn a_streamed_entry_renders_the_same_as_one_that_arrived_whole() {
        // The layout cache is keyed on text length, so an entry that grew by
        // append must not keep showing the render of its shorter self.
        let mut streamed = App::new("main", "b0");
        streamed.push(Who::Model, "");
        for part in TEXT.split_inclusive(' ') {
            streamed
                .pane_mut()
                .transcript
                .last_mut()
                .unwrap()
                .text
                .push_str(part);
            let _ = frame(&mut streamed, 60, 14);
        }

        let mut whole = App::new("main", "b0");
        whole.push(Who::Model, TEXT);

        assert_eq!(frame(&mut streamed, 60, 14), frame(&mut whole, 60, 14));
    }

    #[test]
    fn a_resize_lays_out_again_instead_of_reusing_the_old_width() {
        let mut app = App::new("main", "b0");
        app.push(Who::Model, TEXT);
        let wide = frame(&mut app, 90, 14);
        let narrow = frame(&mut app, 34, 14);

        let mut fresh = App::new("main", "b0");
        fresh.push(Who::Model, TEXT);

        assert_eq!(narrow, frame(&mut fresh, 34, 14));
        assert_ne!(wide, narrow, "a narrower pane must wrap differently");
    }

    #[test]
    fn only_the_visible_rows_are_built() {
        // A transcript far taller than the viewport must still show its tail,
        // which is the property the windowed render could plausibly break.
        let mut long = App::new("main", "b0");
        for i in 0..40 {
            long.push(Who::User, format!("question {i}"));
            long.push(Who::Model, TEXT);
        }
        let seen = frame(&mut long, 60, 14);
        assert!(
            seen.contains("question 39"),
            "tail of the transcript is off screen:\n{seen}"
        );
        assert!(
            !seen.contains("question 0 "),
            "head should have scrolled away"
        );
    }

    #[test]
    fn wide_text_wraps_at_the_margin_instead_of_being_clipped() {
        // Counting characters rather than cells makes a CJK paragraph wrap at
        // roughly twice the intended width; the painter then clips whatever ran
        // past the edge, and the tail is simply gone with nothing on screen to
        // say it was ever there.
        let cjk = "生成式模型的输出需要在终端里正确换行，否则右边的文字会被裁掉。".repeat(4);
        let mixed = format!(
            "Mixed 日本語 text with 絵文字 🎉 and a very long ASCII tail {}",
            "x".repeat(50)
        );

        for who in [Who::Model, Who::User, Who::System] {
            for text in [&cjk, &mixed] {
                for width in [20usize, 33, 40, 61, 80] {
                    let (lines, _) = layout_entry(who, text, width, 235);
                    for line in &lines {
                        let w = crate::md::width_of(&line.to_string());
                        assert!(
                            w <= width,
                            "a {who:?} line is {w} cells at width {width}: {:?}",
                            line.to_string()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_wide_glyph_moves_the_caret_by_two_columns() {
        // The caret is placed in cells but `cursor` counts characters, so a
        // wide glyph is where the two disagree. Getting this wrong puts the
        // caret inside the previous character.
        let mut app = App::new("main", "b0");
        let pane = app.pane_mut();
        pane.set_input("你好ab".into());

        let (rows, caret) = pane.layout_input(20);
        assert_eq!(rows, vec!["你好ab".to_owned()]);
        assert_eq!(
            caret,
            (0, 6),
            "two wide glyphs and two narrow ones is six cells"
        );

        pane.home();
        pane.move_right(false);
        assert_eq!(
            pane.layout_input(20).1,
            (0, 2),
            "one wide glyph is two cells"
        );
    }

    #[test]
    fn input_wraps_on_cells_so_a_wide_line_does_not_overrun_its_box() {
        let mut app = App::new("main", "b0");
        let pane = app.pane_mut();
        pane.set_input("你好世界你好世界".into());
        let (rows, _) = pane.layout_input(9);
        for row in &rows {
            assert!(
                crate::md::width_of(row) <= 9,
                "row {row:?} is {} cells, wider than 9",
                crate::md::width_of(row)
            );
        }
        assert_eq!(
            rows.concat(),
            "你好世界你好世界",
            "wrapping must not drop anything"
        );
    }

    #[test]
    fn the_suggestion_list_appears_above_the_prompt_and_only_for_slashes() {
        let mut app = App::new("main", "b0");
        // A connected pane, so the prompt carries its usual marker and the
        // test can tell which row it is.
        app.apply_transport(Transport::Connected);
        // Nothing typed, nothing suggested: the band takes no rows at all, so
        // the conversation keeps the space.
        assert_eq!(app.menu_rows(), 0);
        let plain = frame(&mut app, 60, 12);
        assert!(
            !plain.contains("/voice"),
            "suggestions shown with no slash typed:\n{plain}"
        );

        app.pane_mut().set_input("/".into());
        assert_eq!(
            app.menu_rows() as usize,
            crate::complete::COMMANDS.len(),
            "a bare slash offers every command"
        );
        let seen = frame(&mut app, 60, 12);
        for cmd in crate::complete::COMMANDS {
            let name = format!("/{}", cmd.name);
            assert!(
                seen.contains(&name),
                "{name} missing from the list:\n{seen}"
            );
        }
        // Above the prompt, not below it: the row carrying the marker is the
        // input, and every suggestion has to sit before it.
        let rows: Vec<&str> = seen.lines().collect();
        let prompt = rows
            .iter()
            .position(|r| r.contains('\u{203a}'))
            .expect("prompt row");
        let voice = rows
            .iter()
            .position(|r| r.contains("/voice"))
            .expect("suggestion row");
        assert!(voice < prompt, "list is below the prompt:\n{seen}");

        // An ordinary message never opens it.
        app.pane_mut().set_input("what is /voice anyway".into());
        assert_eq!(app.menu_rows(), 0);
    }

    #[test]
    fn typing_narrows_the_list_and_tab_completes_the_highlighted_one() {
        let mut app = App::new("main", "b0");
        app.pane_mut().set_input("/v".into());
        assert_eq!(app.menu_rows(), 1, "only /voice starts with v");
        assert!(app.complete_slash());
        // The trailing space is the point: the argument goes straight after.
        assert_eq!(app.pane().input, "/voice ");
        assert_eq!(app.pane().cursor, 7, "caret follows the completion");
        // Completing closes the list, since the name is settled.
        assert_eq!(app.menu_rows(), 0);

        // Nothing to complete is a no-op rather than an inserted tab.
        app.pane_mut().set_input("hello".into());
        assert!(!app.complete_slash());
        assert_eq!(app.pane().input, "hello");
    }

    #[test]
    fn up_and_down_walk_the_list_and_wrap() {
        let mut app = App::new("main", "b0");
        app.pane_mut().set_input("/".into());
        let total = app.menu_rows() as usize;
        assert_eq!(app.menu_index(), 0);
        for i in 1..total {
            assert!(app.menu_move(1));
            assert_eq!(app.menu_index(), i);
        }
        // Wrapping both ways:
        assert!(app.menu_move(1));
        assert_eq!(app.menu_index(), 0);
        assert!(app.menu_move(-1));
        assert_eq!(
            app.menu_index(),
            total - 1,
            "up from the first reaches the last"
        );

        // Tab takes the highlighted entry, not the first.
        assert!(app.complete_slash());
        assert_eq!(app.pane().input, "/help ");
    }

    #[test]
    fn editing_the_line_drops_a_selection_made_against_the_old_one() {
        // The index is meaningless once the list it indexed has changed, and
        // carrying it over would silently complete the wrong command.
        let mut app = App::new("main", "b0");
        app.pane_mut().set_input("/".into());
        app.menu_move(2);
        assert_eq!(app.menu_index(), 2);

        app.pane_mut().set_input("/q".into());
        assert_eq!(app.menu_index(), 0, "a narrowed list starts from the top");
        assert!(app.complete_slash());
        assert_eq!(app.pane().input, "/quit ");
    }

    #[test]
    fn tab_completes_the_name_then_steps_through_the_arguments() {
        // One key, two jobs, never ambiguous: which one applies is decided by
        // whether the command has been named yet.
        let mut app = App::new("main", "b0");
        app.voice = false;
        app.pane_mut().set_input("/v".into());

        assert!(app.complete_slash());
        assert_eq!(app.pane().input, "/voice ", "first press settles the name");

        assert!(app.complete_slash());
        assert_eq!(app.pane().input, "/voice on", "then the toggle");

        assert!(app.complete_slash());
        assert_eq!(app.pane().input, "/voice alba", "then the voices");

        assert!(app.complete_slash());
        assert_eq!(app.pane().input, "/voice marius");

        // The caret trails the argument, so typing continues from the end.
        assert_eq!(app.pane().cursor, app.pane().input.chars().count());

        // A command taking nothing stops after its name rather than cycling.
        app.pane_mut().set_input("/qu".into());
        assert!(app.complete_slash());
        assert_eq!(app.pane().input, "/quit ");
        assert!(!app.complete_slash(), "/quit has nothing to step through");
        assert_eq!(app.pane().input, "/quit ");
    }

    #[test]
    fn the_bar_says_which_job_tab_is_doing() {
        // The dim hint is gone once a real argument is in place, so the bar is
        // what says the key still does something.
        let mut app = App::new("main", "b0");
        app.apply_transport(Transport::Connected);
        app.pane_mut().set_input("/v".into());
        assert!(frame(&mut app, 70, 8).contains("complete"));

        app.pane_mut().set_input("/voice on".into());
        assert!(
            !app.menu_open(),
            "the list is closed once an argument is there"
        );
        assert!(app.args_open());
        assert!(frame(&mut app, 70, 8).contains("arguments"));

        // Back to the ordinary bindings for an ordinary message.
        app.pane_mut().set_input("hello".into());
        assert!(frame(&mut app, 70, 8).contains("history"));
    }

    #[test]
    fn the_arguments_show_dim_once_the_command_is_named() {
        let mut app = App::new("main", "b0");
        app.voice = false;
        app.pane_mut().set_input("/voice".into());
        let off = frame(&mut app, 60, 12);
        assert!(
            off.contains("on"),
            "no argument hint when voice is off:\n{off}"
        );
        assert!(
            !off.contains("off"),
            "offered `off` while already off:\n{off}"
        );
        assert!(off.contains("<name>"), "voice names not offered:\n{off}");

        // The toggle flips to the direction that would now change something.
        app.voice = true;
        let on = frame(&mut app, 60, 12);
        assert!(on.contains("off"), "no `off` offered while speaking:\n{on}");

        // A command taking nothing grows no hint.
        app.pane_mut().set_input("/quit".into());
        let quit = frame(&mut app, 60, 12);
        assert!(quit.contains("/quit"), "input not shown:\n{quit}");
        assert!(!quit.contains("<name>"), "hint leaked onto /quit:\n{quit}");
    }

    /// A pane that has completed a turn costing `tokens`.
    fn with_usage(tokens: u32, window: u32) -> App {
        let mut app = App::new("main", "b0");
        app.apply_transport(Transport::Connected);
        app.model = "gpt-5.6-luna".into();
        app.effort = "none".into();
        app.context_window = window;
        app.apply("main", finished_costing(tokens));
        app
    }

    #[test]
    fn the_bar_names_the_model_and_the_effort() {
        let mut app = with_usage(100, 128_000);
        let seen = frame(&mut app, 100, 8);
        assert!(
            seen.contains("gpt-5.6-luna"),
            "model missing from the bar:\n{seen}"
        );
        assert!(
            seen.contains("none"),
            "effort missing from the bar:\n{seen}"
        );
    }

    #[test]
    fn the_gauge_fills_in_proportion_and_sits_at_the_right_edge() {
        // A tenth of the way along: one cell of eight, and the number to match.
        let mut app = with_usage(12_800, 128_000);
        let seen = frame(&mut app, 100, 8);
        assert!(seen.contains("10%"), "wrong proportion:\n{seen}");
        let bar = seen.lines().last().expect("bar row");
        assert!(
            bar.contains('\u{2588}') && bar.contains('\u{2591}'),
            "no gauge:\n{bar}"
        );
        // Anchored to the right edge rather than trailing the hint.
        let end = bar.trim_end().len();
        assert!(
            end >= 96,
            "gauge is not at the right edge, ends at {end}:\n{bar}"
        );

        // Full is full, and cannot round past the end of the gauge.
        let mut app = with_usage(200_000, 128_000);
        let seen = frame(&mut app, 100, 8);
        assert!(seen.contains("100%"), "over-full should clamp:\n{seen}");
        let filled = seen.lines().last().unwrap().matches('\u{2588}').count();
        assert_eq!(filled, 8, "gauge overflowed its eight cells");
    }

    #[test]
    fn without_a_configured_window_it_reports_the_count_and_claims_nothing() {
        // Inventing a denominator would be worse than showing none: the number
        // would look authoritative and be made up.
        let mut app = with_usage(18_000, 0);
        let seen = frame(&mut app, 100, 8);
        assert!(seen.contains("18k"), "token count missing:\n{seen}");
        assert!(!seen.contains('%'), "a percentage without a limit:\n{seen}");
        assert!(
            !seen.contains('\u{2588}'),
            "a gauge without a limit:\n{seen}"
        );
    }

    #[test]
    fn nothing_is_claimed_before_the_first_turn_completes() {
        let mut app = App::new("main", "b0");
        app.model = "gpt-5.6-luna".into();
        app.context_window = 128_000;
        let seen = frame(&mut app, 100, 8);
        assert!(
            seen.contains("gpt-5.6-luna"),
            "model should show immediately:\n{seen}"
        );
        assert!(
            !seen.contains('%'),
            "a gauge before any usage was reported:\n{seen}"
        );
    }

    #[test]
    fn a_narrow_bar_sheds_the_model_before_the_gauge_and_the_hint_before_neither() {
        let mut app = with_usage(38_000, 128_000);

        // Room for everything.
        let wide = frame(&mut app, 92, 8);
        let bar = wide.lines().last().expect("bar row");
        assert!(bar.contains("gpt-5.6-luna") && bar.contains("30%"), "{bar}");

        // Not quite: the model goes, because it is constant all session while
        // the gauge is the only part that moves.
        let mid = frame(&mut app, 70, 8);
        let bar = mid.lines().last().expect("bar row");
        assert!(
            !bar.contains("gpt-5.6-luna"),
            "model kept over the gauge:\n{bar}"
        );
        assert!(
            bar.contains("30%"),
            "gauge dropped before the model:\n{bar}"
        );

        // No room at all: the hint says what the next keypress does and wins.
        let narrow = frame(&mut app, 50, 8);
        let bar = narrow.lines().last().expect("bar row");
        assert!(
            bar.contains("history"),
            "the hint was the thing dropped:\n{bar}"
        );
        assert!(!bar.contains('%'), "status overlapped the hint:\n{bar}");
    }

    #[test]
    fn a_paste_lands_at_the_caret_with_line_endings_normalised() {
        let mut app = App::new("main", "b0");
        let pane = app.pane_mut();
        pane.set_input("ab".into());
        pane.move_left(false);
        pane.insert_str("X\r\nY\r");
        assert_eq!(pane.input, "aX\nY\nb");
        // The caret follows the pasted run rather than jumping to the end.
        assert_eq!(pane.cursor, 5);
    }

    fn ask(question: &str, options: &[&str], multiple: bool) -> crate::tools::Ask {
        crate::tools::Ask {
            call_id: "call_1".to_owned(),
            question: question.to_owned(),
            options: options.iter().map(|o| (*o).to_owned()).collect(),
            multiple,
        }
    }

    #[test]
    fn an_ask_with_no_options_is_a_lone_text_field_carrying_the_question() {
        // `ask()` accepts `options: []`, so the conversion must stay total
        // over it rather than assuming there is always a `Choice` field.
        let panel = Panel::from(&ask("what should I call the branch?", &[], false));
        assert_eq!(panel.fields.len(), 1);
        assert!(
            matches!(&panel.fields[0], Field::Text { prompt } if prompt == "what should I call the branch?")
        );
    }

    #[test]
    fn an_ask_with_options_gets_a_trailing_free_text_field_too() {
        // The fifth "type an answer" option is always available, per the
        // lead's spec -- it must not depend on how many options came in.
        let panel = Panel::from(&ask("which one?", &["a", "b"], false));
        assert_eq!(panel.fields.len(), 2);
        assert!(
            matches!(&panel.fields[0], Field::Choice { options, .. } if options == &["a", "b"])
        );
        assert!(matches!(&panel.fields[1], Field::Text { .. }));
    }

    #[test]
    fn opening_a_radio_panel_selects_the_first_row_without_a_keypress() {
        // Highlighting a radio option selects it (same convention as the
        // slash-command menu), so the row the panel opens on must already
        // read as chosen -- otherwise Enter with no key pressed would submit
        // an answer nobody chose.
        let panel = Panel::from(&ask("pick one", &["a", "b", "c"], false));
        assert_eq!(
            panel.submit(),
            PanelOutcome::Submitted(vec!["a".to_owned(), String::new()])
        );
    }

    #[test]
    fn up_and_down_move_the_radio_selection_and_wrap() {
        let mut panel = Panel::from(&ask("pick one", &["a", "b"], false));
        assert!(panel.move_row(1));
        assert_eq!(
            panel.submit(),
            PanelOutcome::Submitted(vec!["b".to_owned(), String::new()])
        );
        // Two options plus the trailing text row is three rows; one more
        // step down leaves the radio field and lands on the text row, which
        // does not disturb the choice already made.
        assert!(panel.move_row(1));
        assert_eq!(
            panel.submit(),
            PanelOutcome::Submitted(vec!["b".to_owned(), String::new()])
        );
        // Wraps back to the first row from the last.
        assert!(panel.move_row(1));
        assert_eq!(
            panel.submit(),
            PanelOutcome::Submitted(vec!["a".to_owned(), String::new()])
        );
    }

    #[test]
    fn space_toggles_a_checkbox_row_but_does_nothing_on_a_radio_row() {
        let mut radio = Panel::from(&ask("pick one", &["a", "b"], false));
        assert!(!radio.toggle(), "a radio row has nothing to toggle");
        assert_eq!(
            radio.submit(),
            PanelOutcome::Submitted(vec!["a".to_owned(), String::new()])
        );

        let mut boxes = Panel::from(&ask("pick some", &["a", "b"], true));
        assert_eq!(
            boxes.submit(),
            PanelOutcome::Submitted(vec![String::new(), String::new()])
        );
        assert!(boxes.toggle());
        assert!(boxes.move_row(1));
        assert!(boxes.toggle());
        assert_eq!(
            boxes.submit(),
            PanelOutcome::Submitted(vec!["a, b".to_owned(), String::new()])
        );
        // Toggling again clears it -- not a one-way ratchet.
        assert!(boxes.toggle());
        assert_eq!(
            boxes.submit(),
            PanelOutcome::Submitted(vec!["a".to_owned(), String::new()])
        );
    }

    #[test]
    fn typing_only_lands_on_the_text_row() {
        let mut panel = Panel::from(&ask("pick one", &["a", "b"], false));
        assert!(!panel.type_char('x'), "the radio row is not a text field");
        assert!(
            panel.move_row(2),
            "step past both options onto the text row"
        );
        assert!(panel.type_char('h'));
        assert!(panel.type_char('i'));
        assert_eq!(
            panel.submit(),
            PanelOutcome::Submitted(vec!["a".to_owned(), "hi".to_owned()])
        );
        assert!(panel.backspace());
        assert_eq!(
            panel.submit(),
            PanelOutcome::Submitted(vec!["a".to_owned(), "h".to_owned()])
        );
    }

    #[test]
    fn primary_answer_prefers_the_choice_over_the_trailing_free_text() {
        let mut panel = Panel::from(&ask("pick one", &["a", "b"], false));
        let outcome = panel.submit();
        assert_eq!(outcome.primary_answer(), Some("a"));

        // Move past the options and type instead, without ever toggling a
        // checkbox or landing back on a radio row -- the choice field's
        // answer is still "a" from opening, so it would win unless the free
        // text is meant to override an untouched default. It should not:
        // the user only ever interacted with the text row.
        assert!(panel.move_row(2));
        panel.type_char('z');
        let outcome = panel.submit();
        assert_eq!(outcome.primary_answer(), Some("a"));
    }

    #[test]
    fn a_letter_typed_on_a_choice_row_lands_in_free_text_instead_of_being_dropped() {
        // Reported: a typed character on a choice row reached neither the
        // panel's text field nor the prompt -- it was silently dropped.
        let mut panel = Panel::from(&ask("pick one", &["a", "b"], false));
        assert!(panel.char('h'));
        assert!(panel.char('i'));
        assert_eq!(
            panel.submit(),
            PanelOutcome::Submitted(vec!["a".to_owned(), "hi".to_owned()])
        );
    }

    #[test]
    fn a_digit_selects_the_matching_numbered_option() {
        let mut panel = Panel::from(&ask("pick one", &["a", "b", "c"], false));
        assert!(panel.char('3'), "3 should select the third option");
        assert_eq!(
            panel.submit(),
            PanelOutcome::Submitted(vec!["c".to_owned(), String::new()])
        );
    }

    #[test]
    fn a_digit_toggles_the_matching_option_on_a_checkbox() {
        let mut panel = Panel::from(&ask("pick some", &["a", "b", "c"], true));
        assert!(panel.char('2'));
        assert!(panel.char('3'));
        assert_eq!(
            panel.submit(),
            PanelOutcome::Submitted(vec!["b, c".to_owned(), String::new()])
        );
        // Toggling again clears it -- not a one-way ratchet, same as space.
        assert!(panel.char('2'));
        assert_eq!(
            panel.submit(),
            PanelOutcome::Submitted(vec!["c".to_owned(), String::new()])
        );
    }

    #[test]
    fn a_repeated_digit_stays_handled_even_when_nothing_visibly_changes() {
        // The trap in the implementation: `toggle` returns false for
        // "already exactly this", which is the right answer for "should
        // this repaint" and the wrong one for "did the digit mean
        // something". Pressing the same already-selected digit twice must
        // not fall through and type a literal '1' into free text.
        let mut panel = Panel::from(&ask("pick one", &["a", "b"], false));
        assert!(
            panel.char('1'),
            "1 re-selects the option the panel opened on"
        );
        assert_eq!(
            panel.submit(),
            PanelOutcome::Submitted(vec!["a".to_owned(), String::new()])
        );
    }

    #[test]
    fn a_digit_with_no_matching_option_falls_through_to_free_text() {
        let mut panel = Panel::from(&ask("pick one", &["a", "b"], false));
        // Only two options: 5 names nothing, so it must not be silently
        // dropped -- it lands as a literal '5' in the free-text field.
        assert!(panel.char('5'));
        assert_eq!(
            panel.submit(),
            PanelOutcome::Submitted(vec!["a".to_owned(), "5".to_owned()])
        );
    }

    #[test]
    fn a_digit_stays_literal_once_the_highlight_is_on_free_text() {
        // "how many retries?" must still be answerable "3" -- digit-select
        // applies only while the highlight is on a choice row, never once
        // it has reached free text.
        let mut panel = Panel::from(&ask("how many retries?", &[], false));
        assert!(panel.char('3'));
        assert_eq!(
            panel.submit(),
            PanelOutcome::Submitted(vec!["3".to_owned()])
        );
    }

    #[test]
    fn digit_select_on_one_question_does_not_disturb_an_answer_already_given_to_another() {
        // The property `move_row`'s restraint protects, checked against
        // digit-select specifically: an explicit number press answering
        // question 2 must not rewrite question 1's already-given answer.
        let mut panel = Panel::from(&ask("first?", &["a", "b"], false));
        panel.extend_with(&ask2("second?", &["x", "y"], false));

        assert!(panel.move_row(1), "answer q1 with b");
        assert!(
            panel.move_row(3),
            "cross q1's free text onto q2's own choice row"
        );
        assert!(panel.char('2'), "2 should select q2's second option");

        assert_eq!(
            panel.submit_by_call(),
            vec![
                ("call_1".to_owned(), "b".to_owned()),
                ("call_2".to_owned(), "y".to_owned())
            ],
            "an explicit digit press for q2 must not disturb q1's already-given answer"
        );
    }

    #[test]
    fn primary_answer_falls_back_to_free_text_when_there_is_no_choice_field() {
        let mut panel = Panel::from(&ask("what should I call it?", &[], false));
        panel.type_char('x');
        assert_eq!(panel.submit().primary_answer(), Some("x"));
    }

    #[test]
    fn cancelling_a_panel_reports_no_answer() {
        assert_eq!(PanelOutcome::Cancelled.primary_answer(), None);
    }

    #[test]
    fn open_panel_move_and_cancel_round_trip_through_app() {
        let mut app = App::new("main", "b0");
        assert!(!app.panel_open());
        assert!(
            app.cancel_panel().is_none(),
            "nothing open, nothing to cancel"
        );

        app.open_panel(Panel::from(&ask("pick one", &["a", "b"], false)));
        assert!(app.panel_open());
        assert!(app.panel_move(1));
        let token = app.cancel_panel();
        assert_eq!(token.as_deref(), Some("call_1"));
        assert!(!app.panel_open(), "cancel closes the panel");
        assert!(
            app.submit_panel().is_empty(),
            "nothing left to submit after cancel"
        );
    }

    #[test]
    fn submit_panel_answers_each_call_once() {
        let mut app = App::new("main", "b0");
        app.open_panel(Panel::from(&ask("pick one", &["a", "b"], true)));
        assert!(app.panel_toggle(), "the radio-turned-checkbox row toggles");
        // One answer for the call, not one per row: the choice and the
        // free-text fallback beneath it are two ways to answer one question.
        assert_eq!(
            app.submit_panel(),
            vec![("call_1".to_owned(), "a".to_owned())]
        );
        assert!(!app.panel_open());
    }

    #[test]
    fn the_panel_renders_its_question_options_and_highlighted_row() {
        let mut app = App::new("main", "b0");
        app.open_panel(Panel::from(&ask("continue?", &["yes", "no"], false)));
        let screen = frame(&mut app, 60, 14);
        assert!(
            screen.contains("continue?"),
            "question not shown:\n{screen}"
        );
        assert!(screen.contains("yes"), "option not shown:\n{screen}");
        assert!(screen.contains("no"), "option not shown:\n{screen}");
        assert!(
            screen.contains("type an answer"),
            "fifth option not shown:\n{screen}"
        );
        assert!(
            screen.contains('\u{2022}'),
            "the opening selection is not marked:\n{screen}"
        );

        // The hint bar names the panel's own bindings, not the ordinary
        // send-mode ones, while it is up.
        assert!(screen.contains("submit"), "panel hint not shown:\n{screen}");
        assert!(
            !screen.contains("history"),
            "send-mode hint leaked through:\n{screen}"
        );
    }

    #[test]
    fn the_panel_takes_the_keyboard_over_ordinary_typing() {
        // A regression guard for the precedence in `main.rs::handle_key`: a
        // plain character while the panel is open must not fall through to
        // `pane.input`, or the model behind the parked turn would see a
        // typed answer land in the next message instead of the tool result.
        let mut app = App::new("main", "b0");
        app.open_panel(Panel::from(&ask("pick one", &["a", "b"], false)));
        // Whatever the panel does with the key, it must never reach the
        // ordinary prompt buffer underneath it.
        app.panel_type('x');
        assert_eq!(app.pane().input, "", "ordinary input must be untouched");
    }

    #[test]
    fn text_of_reports_the_live_typed_value_and_nothing_else() {
        let mut panel = Panel::from(&ask("pick one", &["a", "b"], false));
        assert_eq!(
            panel.text_of(0),
            "",
            "a Choice field has no text of its own"
        );
        assert!(panel.move_row(2));
        panel.type_char('h');
        panel.type_char('i');
        assert_eq!(panel.text_of(1), "hi");
    }

    #[test]
    fn chosen_reports_only_the_options_actually_selected() {
        let panel = Panel::from(&ask("pick one", &["a", "b"], false));
        assert!(
            panel.chosen(0, 0),
            "opening a radio field selects its first row"
        );
        assert!(!panel.chosen(0, 1), "the second option was never touched");
    }

    /// A panel with one choice field and the free-text field that always
    /// follows it, with the highlight moved onto the text row.
    fn text_panel() -> App {
        let mut app = App::new("main", "b0");
        app.open_panel(Panel::new(
            "call_1".to_owned(),
            vec![
                Field::Choice {
                    prompt: "Pick".into(),
                    options: vec!["a".into()],
                    multi: false,
                },
                Field::Text {
                    prompt: "type an answer".into(),
                },
            ],
        ));
        // Down off the single choice row onto the text row.
        app.panel_move(1);
        app
    }

    #[test]
    fn the_free_text_field_takes_the_message_inputs_keys() {
        // It had `push` and `pop` and nothing else, so a question asking for a
        // sentence could not be answered with one: space toggled the choice
        // above instead of typing, and there was no way back to fix a typo.
        let mut app = text_panel();
        for c in "hello world".chars() {
            assert!(app.panel_type(c), "typing {c:?} did nothing");
        }
        assert_eq!(app.panel_answer_text(), Some("hello world".to_owned()));

        // Word motion, then type in the middle.
        assert!(app.panel_move_left(true), "word-left did nothing");
        for c in "big ".chars() {
            app.panel_type(c);
        }
        assert_eq!(app.panel_answer_text(), Some("hello big world".to_owned()));

        // The readline kills, from the caret rather than the end.
        assert!(app.panel_home());
        assert!(app.panel_kill_to_end());
        assert_eq!(app.panel_answer_text(), Some(String::new()));
    }

    #[test]
    fn panel_char_reaches_the_open_panel_and_is_a_no_op_with_none_open() {
        // `panel_char` is what `main.rs::handle_key` actually calls; every
        // other test in this module exercises `Panel::char` directly, which
        // proves the panel's own logic but not that this wrapper reaches it.
        let mut app = App::new("main", "b0");
        assert!(!app.panel_char('1'), "no panel open, nothing to reach");

        app.open_panel(Panel::from(&ask("pick one", &["a", "b", "c"], false)));
        assert!(
            app.panel_char('3'),
            "3 should select the panel's third option"
        );
        assert_eq!(
            app.submit_panel(),
            vec![("call_1".to_owned(), "c".to_owned())]
        );
    }

    #[test]
    fn space_types_on_a_text_row_and_toggles_on_a_choice_row() {
        // One key, two meanings, decided by where the highlight is -- which is
        // what lets the panel avoid having a mode.
        let mut app = text_panel();
        app.panel_type(' ');
        assert_eq!(
            app.panel_answer_text(),
            Some(" ".to_owned()),
            "space did not type"
        );

        // Back on the choice row it toggles instead. A checkbox, because a
        // radio the panel already selected on arrival is genuinely unchanged
        // by being selected again -- and reporting a change there would cost a
        // repaint for nothing.
        let mut app = App::new("main", "b0");
        app.open_panel(Panel::new(
            "call_1".to_owned(),
            vec![Field::Choice {
                prompt: "Pick".into(),
                options: vec!["a".into(), "b".into()],
                multi: true,
            }],
        ));
        assert!(app.panel_toggle(), "space should toggle a checkbox");
        assert!(app.panel_toggle(), "and toggle it back");
    }

    #[test]
    fn a_key_that_changes_nothing_reports_nothing_so_it_costs_no_repaint() {
        let mut app = text_panel();
        assert!(
            !app.panel_backspace(),
            "backspace on an empty field changed nothing"
        );
        assert!(
            !app.panel_move_left(false),
            "left at the start changed nothing"
        );
        assert!(!app.panel_home(), "home at the start changed nothing");
        assert!(
            !app.panel_kill_to_end(),
            "kill-to-end on an empty field changed nothing"
        );
        app.panel_type('x');
        assert!(
            !app.panel_move_right(false),
            "right at the end changed nothing"
        );
    }

    #[test]
    fn a_paste_lands_in_the_panel_field_whole() {
        let mut app = text_panel();
        assert!(app.panel_paste("two words"));
        assert_eq!(app.panel_answer_text(), Some("two words".to_owned()));
    }

    #[test]
    fn panel_move_reports_false_when_there_is_no_panel_to_move() {
        let mut app = App::new("main", "b0");
        assert!(!app.panel_move(1));
    }

    #[test]
    fn panel_type_only_succeeds_once_a_text_row_has_focus() {
        let mut app = App::new("main", "b0");
        app.open_panel(Panel::from(&ask("pick one", &["a", "b"], false)));
        assert!(!app.panel_type('x'), "a radio row is focused first");
        assert!(app.panel_move(2), "step onto the trailing text row");
        assert!(app.panel_type('y'), "now there is somewhere for it to land");
    }

    #[test]
    fn panel_backspace_only_succeeds_when_there_is_something_typed() {
        let mut app = App::new("main", "b0");
        app.open_panel(Panel::from(&ask("what should I call it?", &[], false)));
        assert!(!app.panel_backspace(), "nothing typed yet");
        app.panel_type('a');
        assert!(app.panel_backspace());
    }

    #[test]
    fn only_the_highlighted_panel_row_carries_the_selection_background() {
        let mut app = App::new("main", "b0");
        app.open_panel(Panel::from(&ask("pick one", &["a", "b"], false)));
        let area = Rect::new(0, 0, 40, 14);
        let mut buffer = Buffer::empty(area);
        app.render(&mut buffer, area);
        // The heading sits one row above the options (see `render_panel`),
        // so the first option row is the second row of the panel strip.
        // No input row: the prompt is hidden while this pane has a question
        // open, so the panel sits directly above the bar.
        let panel_top = area.height - 1 /* bar */ - 3 /* two options + text row */;
        let selected_bg = buffer[(2, panel_top)].bg;
        let other_bg = buffer[(2, panel_top + 1)].bg;
        assert_eq!(
            selected_bg,
            Color::Indexed(238),
            "the opening selection is not tinted"
        );
        assert_ne!(
            other_bg,
            Color::Indexed(238),
            "an unselected row must not carry the tint too"
        );
    }

    fn ask2(question: &str, options: &[&str], multiple: bool) -> crate::tools::Ask {
        crate::tools::Ask {
            call_id: "call_2".to_owned(),
            question: question.to_owned(),
            options: options.iter().map(|o| (*o).to_owned()).collect(),
            multiple,
        }
    }

    fn ask_call(call_id: &str) -> Vec<Incoming> {
        tool_call(call_id, "ask", r#"{"question":"Which?"}"#)
    }

    /// Drives a whole call through `apply` and hands back the effect the
    /// boundary produced. A call is three events now, and only the last one
    /// is a call -- so a test that applied just one would be asserting about
    /// half a fact.
    fn run_call(app: &mut App, lane: &str, events: Vec<Incoming>) -> Effect {
        let mut last = Effect::None;
        for e in events {
            last = app.apply(lane, e);
        }
        last
    }

    #[test]
    fn two_questions_asked_in_one_turn_share_a_panel_and_are_answered_separately() {
        // Reported: asking for two things produced two panels, one after the
        // other. The second call was being refused and retried, which turned
        // one form into a conversation.
        let mut app = App::new("main", "b0");
        app.apply_transport(Transport::Connected);
        app.park_ask("main", ask("first?", &["a", "b"], false));
        app.park_ask("main", ask2("second?", &["x", "y"], false));

        // One panel, both questions on it.
        assert!(app.panel_open(), "a panel should be showing");
        let seen = frame(&mut app, 70, 20);
        for text in ["first?", "second?", "a", "x"] {
            assert!(
                seen.contains(text),
                "{text:?} missing from the shared panel:\n{seen}"
            );
        }
        assert_eq!(
            app.panel().expect("open").calls(),
            vec!["call_1".to_owned(), "call_2".to_owned()],
            "the panel should be answering both calls"
        );

        // Rows: q1 a, q1 b, q1 text, q2 x, q2 y, q2 text.
        // Walking within the first question moves its answer to `b`; crossing
        // into the second answers that one, because it has no answer yet.
        for _ in 0..3 {
            assert!(
                app.panel_move(1),
                "the highlight should reach the second question"
            );
        }

        // Back into the first question and forward again. This is the case that
        // multi-question panels break if landing always selects: crossing back
        // would rewrite an answer already given to whatever was passed last.
        for _ in 0..2 {
            app.panel_move(-1);
        }
        for _ in 0..2 {
            app.panel_move(1);
        }

        let answers = app.submit_panel();
        assert_eq!(
            answers,
            vec![
                ("call_1".to_owned(), "b".to_owned()),
                ("call_2".to_owned(), "x".to_owned())
            ],
            "each call answered once, and neither disturbed by walking past the other"
        );
    }

    #[test]
    fn park_ask_lands_on_the_lane_that_asked_not_the_focused_pane() {
        // Regression: `main.rs` used to open the panel via `open_panel`, which
        // writes to the *focused* pane, then call `park_ask`, which targets
        // the pane owning the lane. With a fork focused those differ, and the
        // question would render on the wrong split -- or, once `open_panel`
        // is dropped, `park_ask` alone must still get this right.
        let mut app = App::new("main", "b0");
        app.panes.push(Pane::new("fork-1", "b1"));
        app.active = 1;

        app.park_ask("main", ask("which?", &["a", "b"], false));

        assert!(
            app.panes[0].panel.is_some(),
            "the asking pane should carry the panel"
        );
        assert!(
            app.panes[1].panel.is_none(),
            "the focused pane must not receive it"
        );
    }

    #[test]
    fn cancelling_a_shared_panel_releases_every_turn_waiting_on_it() {
        // One escape has to answer both, or the unanswered call waits forever
        // on a panel that is no longer on screen.
        let mut app = App::new("main", "b0");
        app.park_ask("main", ask("first?", &["a"], false));
        app.park_ask("main", ask2("second?", &["x"], false));
        assert_eq!(
            app.cancel_panel_calls(),
            vec!["call_1".to_owned(), "call_2".to_owned()]
        );
        assert!(!app.panel_open());
    }

    #[test]
    fn the_raw_tool_traffic_is_shown_only_when_asked_for() {
        // What a reader wants by default is the exchange, not the call id and
        // the JSON that carried it.
        let mut quiet = App::new("main", "b0");
        run_call(&mut quiet, "main", ask_call("call_1"));
        let seen = frame(&mut quiet, 70, 12);
        assert!(
            !seen.contains("call_1"),
            "raw traffic leaked without --debug:\n{seen}"
        );
        assert!(
            !seen.contains("question"),
            "raw arguments leaked without --debug:\n{seen}"
        );

        let mut loud = App::new("main", "b0");
        loud.debug = true;
        run_call(&mut loud, "main", ask_call("call_1"));
        let seen = frame(&mut loud, 70, 12);
        assert!(
            seen.contains("call_1"),
            "--debug should show the call id:\n{seen}"
        );
        assert!(
            seen.contains("ask"),
            "--debug should name the tool:\n{seen}"
        );
    }

    #[test]
    fn an_answered_question_joins_the_conversation_as_two_ordinary_messages() {
        // Asking is something the assistant did and answering is something the
        // user did, so the transcript should read as that exchange rather than
        // as the plumbing that carried it.
        let mut app = App::new("main", "b0");
        app.record_answer("main", "What are you looking forward to?", "a quiet week");

        let who: Vec<Who> = app.pane().transcript.iter().map(|e| e.who).collect();
        assert_eq!(
            who,
            vec![Who::Model, Who::User],
            "question then answer, in that order"
        );
        assert_eq!(
            app.pane().transcript[0].text,
            "What are you looking forward to?"
        );
        assert_eq!(app.pane().transcript[1].text, "a quiet week");

        let seen = frame(&mut app, 70, 12);
        assert!(
            seen.contains("quiet week"),
            "the answer is not in the transcript:\n{seen}"
        );
    }

    #[test]
    fn a_question_answered_with_nothing_leaves_only_the_question() {
        // A declined or empty answer should not post a blank user message,
        // which would read as the user having sent an empty turn.
        let mut app = App::new("main", "b0");
        app.record_answer("main", "Anything?", "   ");
        let who: Vec<Who> = app.pane().transcript.iter().map(|e| e.who).collect();
        assert_eq!(who, vec![Who::Model]);
    }

    #[test]
    fn an_answer_lands_in_the_pane_that_asked_not_the_focused_one() {
        // With splits the user may have moved on before submitting, and the
        // exchange belongs to the conversation it happened in.
        let mut app = App::new("main", "b0");
        app.panes.push(Pane::new("fork-1", "b1"));
        app.active = 1;
        app.record_answer("main", "Which?", "this one");
        assert_eq!(
            app.panes[0].transcript.len(),
            2,
            "the asking pane should have the exchange"
        );
        assert!(
            app.panes[1].transcript.is_empty(),
            "the focused pane must not receive it"
        );
    }

    #[test]
    fn the_prompt_is_hidden_while_this_pane_has_a_question_and_returns_with_the_split() {
        // Typing goes to the panel, so leaving an inert prompt below it invites
        // answering into the wrong place. Per pane, because a fork is a
        // separate conversation: a question parked in one must not silence the
        // other.
        let marker = '\u{203a}';
        let mut app = App::new("main", "b0");
        app.apply_transport(Transport::Connected);
        assert!(
            frame(&mut app, 40, 14).contains(marker),
            "the prompt should start visible"
        );

        app.open_panel(Panel::from(&ask("pick one", &["a", "b"], false)));
        assert!(
            !frame(&mut app, 40, 14).contains(marker),
            "the prompt is still showing under an open panel"
        );

        // A second split with no question of its own keeps its prompt.
        app.pane_mut().selected = Some(0);
        app.panes.push(Pane::new("fork-1", "b1"));
        app.active = 1;
        app.apply_transport(Transport::Connected);
        let seen = frame(&mut app, 40, 14);
        assert!(
            seen.contains(marker),
            "a split without a question lost its prompt to another split's panel:\n{seen}"
        );
    }

    #[test]
    fn connected_reaches_every_pane_not_just_the_default_lane() {
        // Regression: a connection-wide event named no lane, but no pane is
        // ever created with lane "" -- `main.rs` names the first pane
        // "main". `apply`'s per-lane lookup used to gate on that lane before
        // dispatching at all, so Connected always missed and every pane sat
        // frozen at whatever status it started in. It is now structurally
        // impossible: a transport frame carries no lane to get wrong.
        let mut app = App::new("main", "b0");
        app.panes.push(Pane::new("fork-1", "b1"));
        // A fresh pane is already `Connecting`; nothing to set.

        app.apply_transport(Transport::Connected);

        for p in &app.panes {
            assert!(
                p.status() == Status::Ready,
                "every pane should have left Connecting"
            );
        }
    }

    #[test]
    fn the_bar_offers_interrupt_only_while_something_is_outstanding() {
        // The hint is how the user learns interrupt exists at the moment it
        // is useful. Offered when nothing is running it is a lie; withheld
        // when something is, the only way out of a long turn is undiscovered.
        let mut app = App::new("main", "b0");
        app.apply_transport(Transport::Connected);
        assert!(
            !frame(&mut app, 60, 8).contains("esc esc interrupt"),
            "offered an interrupt with nothing to interrupt"
        );

        app.pane_mut().sent_request();
        assert!(
            frame(&mut app, 60, 8).contains("esc esc interrupt"),
            "no way to learn about interrupt while a turn runs"
        );

        // And it goes away again when the turn does, rather than sticking.
        app.apply("main", finished(None));
        assert!(!frame(&mut app, 60, 8).contains("esc esc interrupt"));
    }

    #[test]
    fn a_rewind_clears_the_index_into_the_transcript_it_just_truncated() {
        // `rewind` truncates and used to leave `open_model` pointing into
        // what it removed. Nothing crashed, because `get_mut` returns `None`
        // for an out-of-range index -- but the slot can be refilled, and then
        // the index is live again and pointing at somebody else's entry.
        let mut app = App::new("main", "b0");
        app.apply_transport(Transport::Connected);
        app.push(Who::User, "ask me something".to_owned());
        app.apply("main", text("partial repl"));
        assert_eq!(
            app.pane().open_model,
            Some(1),
            "the turn is streaming into entry 1"
        );

        app.pane_mut().selected = Some(0);
        assert!(app.rewind());
        assert!(app.pane().transcript.is_empty());
        assert_eq!(
            app.pane().open_model,
            None,
            "an index outlived what it pointed at"
        );
    }

    #[test]
    fn a_stale_open_model_index_cannot_capture_an_entry_that_is_not_the_model_s() {
        // The partner to the test above, and it stays even though `rewind` no
        // longer produces this state: the guard is defence against a class,
        // not a fix for the one path that reached it. The index is set by
        // hand here for exactly that reason -- what matters is what happens
        // when one is stale, not which caller made it so.
        //
        // The consequence it prevents is a silent data-integrity failure: the
        // model's text appended to the user's own message, where it reads as
        // something the user said.
        let mut app = App::new("main", "b0");
        app.apply_transport(Transport::Connected);
        app.record_answer("main", "Which one?", "the second");
        assert_eq!(app.pane().transcript[1].who, Who::User);
        app.pane_mut().open_model = Some(1);

        app.apply("main", text("continuing"));
        assert_eq!(
            app.pane().transcript[1].text,
            "the second",
            "the model's text was appended to the user's own message"
        );
        assert_eq!(
            app.pane()
                .transcript
                .last()
                .map(|e| (e.who, e.text.clone())),
            Some((Who::Model, "continuing".to_owned())),
            "and it must land in a model entry of its own"
        );
    }

    #[test]
    fn a_tool_call_is_assembled_from_its_three_events_and_dispatched_only_at_the_end() {
        // The provider hands a call over whole and AG-UI splits it in three,
        // so `App` is what puts it back together. The assertion is that
        // nothing dispatches early: a call acted on at `TOOL_CALL_START` runs
        // with no arguments at all, which for `file_read` is a read of no
        // path and for `ask` is a question with no text.
        let mut app = App::new("main", "b0");
        app.apply_transport(Transport::Connected);
        let mut events = tool_call("c1", "file_read", r#"{"path":"a.txt"}"#).into_iter();

        assert_eq!(
            app.apply("main", events.next().unwrap()),
            Effect::None,
            "start is not a call"
        );
        assert_eq!(
            app.apply("main", events.next().unwrap()),
            Effect::None,
            "args are not a call"
        );
        assert_eq!(
            app.apply("main", events.next().unwrap()),
            Effect::RunTool(crate::tools::Call {
                id: "c1".to_owned(),
                name: "file_read".to_owned(),
                arguments: r#"{"path":"a.txt"}"#.to_owned(),
            }),
            "the end of the call is the call"
        );
        assert!(
            app.pane().open_calls.is_empty(),
            "the call was not taken off the pane"
        );
    }

    #[test]
    fn arguments_split_across_several_events_are_concatenated_in_order() {
        // One event carries the whole thing today and the protocol permits
        // any number, so a foreign agent streaming its arguments must not
        // produce a call with only the last fragment -- which would be
        // syntactically valid JSON often enough to be confusing.
        let mut app = App::new("main", "b0");
        app.apply_transport(Transport::Connected);
        app.apply(
            "main",
            Incoming::ToolCallStart {
                base: Default::default(),
                tool_call_id: "c1".to_owned(),
                tool_call_name: "file_read".to_owned(),
                parent_message_id: None,
            },
        );
        for fragment in [r#"{"pa"#, r#"th":"#, r#""a.txt"}"#] {
            app.apply(
                "main",
                Incoming::ToolCallArgs {
                    base: Default::default(),
                    tool_call_id: "c1".to_owned(),
                    delta: fragment.to_owned(),
                },
            );
        }
        let end = app.apply(
            "main",
            Incoming::ToolCallEnd {
                base: Default::default(),
                tool_call_id: "c1".to_owned(),
            },
        );
        let Effect::RunTool(call) = end else {
            panic!("no call: {end:?}")
        };
        assert_eq!(call.arguments, r#"{"path":"a.txt"}"#);
    }

    #[test]
    fn two_calls_open_at_once_keep_their_own_arguments() {
        // Interleaving is legal and the ids are the only thing keeping them
        // apart. With one slot instead of a list the second call's arguments
        // land on the first, and both run with the wrong input -- silently,
        // because both are still valid JSON.
        let mut app = App::new("main", "b0");
        app.apply_transport(Transport::Connected);
        for (id, name) in [("c1", "file_read"), ("c2", "ask")] {
            app.apply(
                "main",
                Incoming::ToolCallStart {
                    base: Default::default(),
                    tool_call_id: id.to_owned(),
                    tool_call_name: name.to_owned(),
                    parent_message_id: None,
                },
            );
        }
        for (id, args) in [
            ("c2", r#"{"question":"Which?"}"#),
            ("c1", r#"{"path":"a.txt"}"#),
        ] {
            app.apply(
                "main",
                Incoming::ToolCallArgs {
                    base: Default::default(),
                    tool_call_id: id.to_owned(),
                    delta: args.to_owned(),
                },
            );
        }
        let Effect::RunTool(first) = app.apply(
            "main",
            Incoming::ToolCallEnd {
                base: Default::default(),
                tool_call_id: "c1".to_owned(),
            },
        ) else {
            panic!("c1 did not close")
        };
        assert_eq!(first.name, "file_read");
        assert_eq!(first.arguments, r#"{"path":"a.txt"}"#);
        assert_eq!(
            app.pane().open_calls.len(),
            1,
            "closing one must not close the other"
        );

        let Effect::RunTool(second) = app.apply(
            "main",
            Incoming::ToolCallEnd {
                base: Default::default(),
                tool_call_id: "c2".to_owned(),
            },
        ) else {
            panic!("c2 did not close")
        };
        assert_eq!(second.name, "ask");
        assert_eq!(second.arguments, r#"{"question":"Which?"}"#);
    }

    #[test]
    fn an_end_or_arguments_for_a_call_nobody_opened_produce_nothing() {
        // A misbehaving agent, and the failure to avoid is inventing a call
        // from a fragment: there would be no name to dispatch it to, and the
        // turn would then wait for a result nobody is producing.
        let mut app = App::new("main", "b0");
        app.apply_transport(Transport::Connected);
        assert_eq!(
            app.apply(
                "main",
                Incoming::ToolCallArgs {
                    base: Default::default(),
                    tool_call_id: "ghost".to_owned(),
                    delta: "{}".to_owned(),
                },
            ),
            Effect::None
        );
        assert_eq!(
            app.apply(
                "main",
                Incoming::ToolCallEnd {
                    base: Default::default(),
                    tool_call_id: "ghost".to_owned()
                },
            ),
            Effect::None
        );
        assert!(app.pane().open_calls.is_empty());
    }

    #[test]
    fn a_chunk_event_is_read_as_content_because_a_conforming_agent_may_send_nothing_else() {
        // The specification's own reference server streams OpenAI through
        // the chunk events exclusively, so this is the canonical conforming
        // agent rather than an edge case. Both spellings must reach the
        // transcript, and the second assertion is what stops a normalisation
        // that swallowed the ordinary form from passing.
        let mut app = App::new("main", "b0");
        app.apply_transport(Transport::Connected);
        app.apply(
            "main",
            Incoming::TextMessageChunk {
                base: Default::default(),
                message_id: Some("m".to_owned()),
                role: None,
                delta: Some("from a chunk".to_owned()),
            },
        );
        assert_eq!(
            app.pane()
                .transcript
                .last()
                .map(|e| e.text.clone())
                .unwrap_or_default(),
            "from a chunk"
        );

        app.apply("main", text(" and from content"));
        assert_eq!(
            app.pane()
                .transcript
                .last()
                .map(|e| e.text.clone())
                .unwrap_or_default(),
            "from a chunk and from content",
            "both spellings must land in the same open model entry"
        );
    }

    #[test]
    fn a_chunk_carrying_no_delta_adds_nothing_rather_than_an_empty_turn() {
        // The corpus has exactly this: a `TEXT_MESSAGE_CHUNK` with no fields
        // set at all. Treating it as content opens a model entry with no
        // text, which renders as a blank reply.
        let mut app = App::new("main", "b0");
        app.apply_transport(Transport::Connected);
        app.apply(
            "main",
            Incoming::TextMessageChunk {
                base: Default::default(),
                message_id: None,
                role: None,
                delta: None,
            },
        );
        assert!(
            app.pane().transcript.is_empty(),
            "an empty chunk opened a turn"
        );
    }

    #[test]
    fn a_run_that_reports_usage_moves_the_gauge_and_one_that_does_not_leaves_it() {
        // Fold B arriving where it is used. `None` leaving the gauge alone is
        // the half that matters: a run reporting nothing is not a run that
        // was free, and the previous seam type read an AG-UI usage object as
        // three zeros.
        let mut app = App::new("main", "b0");
        app.apply_transport(Transport::Connected);
        assert_eq!(app.pane().context, None);

        app.apply("main", finished_costing(1234));
        assert_eq!(app.pane().context, Some(1234));

        app.apply("main", finished(None));
        assert_eq!(
            app.pane().context,
            Some(1234),
            "a report of nothing overwrote a real count"
        );
    }

    #[test]
    fn a_run_that_ends_without_an_id_chains_from_nothing_rather_than_from_an_empty_string() {
        // `run_id` is required by the specification where our `response_id`
        // was optional, so an absent one arrives as "". Storing that would
        // send `previous_response_id: ""` on the next turn, which is a chain
        // to nowhere and an error the provider reports rather than ignores.
        let mut app = App::new("main", "b0");
        app.apply_transport(Transport::Connected);
        app.apply("main", finished(Some("resp_1")));
        assert_eq!(app.pane().last_response_id.as_deref(), Some("resp_1"));

        app.apply("main", finished(None));
        assert_eq!(
            app.pane().last_response_id,
            None,
            "an empty run id became a chain"
        );
    }

    #[test]
    fn a_pane_is_waiting_for_as_long_as_anything_is_outstanding() {
        // The invariant itself, stated once: `Waiting` belongs to whoever
        // has a request outstanding, and only a terminal update for that
        // request clears it. Two requests, two terminals, and the pane is
        // not idle in between -- which is the whole thing a flag could not
        // say.
        let mut app = App::new("main", "b0");
        app.apply_transport(Transport::Connected);
        assert_eq!(app.pane().status(), Status::Ready);

        app.pane_mut().sent_request();
        app.pane_mut().sent_request();
        assert_eq!(app.pane().status(), Status::Waiting);

        app.apply("main", finished(None));
        assert_eq!(
            app.pane().status(),
            Status::Waiting,
            "one settled, one still outstanding"
        );

        app.apply("main", finished(None));
        assert_eq!(app.pane().status(), Status::Ready, "both settled");
    }

    #[test]
    fn a_pane_is_troubled_by_a_dead_transport_or_a_rejected_turn_and_by_nothing_else() {
        // Both halves need their own case. A dropped transport leaves
        // `failed` false and a rejected turn leaves the link up, so a check
        // that collapsed to either one alone would still pass the other's
        // test -- and reporting an error bar over an ordinary reply is the
        // failure this guards.
        let mut app = App::new("main", "b0");
        app.apply_transport(Transport::Connected);
        assert!(!app.any_pane_failed(), "an idle connected pane is fine");

        app.pane_mut().sent_request();
        assert!(
            !app.any_pane_failed(),
            "waiting on a reply is not a failure"
        );

        app.apply(
            "main",
            Incoming::RunError {
                base: Default::default(),
                message: "no".to_owned(),
                code: Some("bad_request".to_owned()),
                usage: None,
            },
        );
        assert_eq!(app.pane().status(), Status::Ready, "the link is still up");
        assert!(app.any_pane_failed(), "a rejected turn counts");

        app.apply("main", finished(None));
        assert!(!app.any_pane_failed(), "a good turn clears it");

        app.apply_transport(Transport::Disconnected("adapter exited".into()));
        assert!(!app.pane().failed, "nothing rejected the turn");
        assert!(app.any_pane_failed(), "a dead transport counts on its own");
    }

    #[test]
    fn a_request_sent_before_the_transport_is_up_still_reads_as_waiting() {
        // Typing before `Connected` arrives is ordinary at startup, and it
        // is the window the first of these bugs lived in. The request is
        // genuinely outstanding, so the pane says so rather than reporting
        // the transport's state over the user's.
        let mut app = App::new("main", "b0");
        assert_eq!(app.pane().status(), Status::Connecting);
        app.pane_mut().sent_request();
        assert_eq!(app.pane().status(), Status::Waiting);
    }

    #[test]
    fn an_interrupt_abandons_what_was_outstanding_and_a_late_terminal_cannot_undo_it() {
        // Abandoned, not merely finished: Kobold has stopped waiting on the
        // response and discards what comes back, so nothing is outstanding
        // from its point of view. The server does not know that and still
        // sends a terminal event, which must not push the count below zero
        // -- wrapping there would leave a pane waiting forever on requests
        // that finished long ago.
        let mut app = App::new("main", "b0");
        app.apply_transport(Transport::Connected);
        app.pane_mut().sent_request();
        app.pane_mut().sent_request();
        assert!(app.interrupt(), "an interrupt should take");
        assert_eq!(
            app.pane().status(),
            Status::Ready,
            "interrupting stops the waiting"
        );

        // The tails of both abandoned requests turn up.
        app.apply("main", finished(None));
        app.apply("main", finished(None));
        app.apply("main", finished(None));
        assert_eq!(
            app.pane().status(),
            Status::Ready,
            "a late terminal wrapped the count"
        );

        // And the pane still works afterwards, which is what an underflow
        // would have broken silently.
        app.pane_mut().sent_request();
        assert_eq!(app.pane().status(), Status::Waiting);
    }

    #[test]
    fn a_dropped_transport_abandons_what_was_outstanding_so_a_reconnect_starts_clean() {
        // No terminal event is ever coming for a request the socket died
        // under, so a count kept across the drop would leak and the pane
        // would come back from a reconnect already waiting on nothing.
        let mut app = App::new("main", "b0");
        app.apply_transport(Transport::Connected);
        app.pane_mut().sent_request();
        app.apply_transport(Transport::Disconnected(
            "server closed the connection".to_owned(),
        ));
        assert_eq!(app.pane().status(), Status::Gone);

        app.apply_transport(Transport::Connected);
        assert_eq!(
            app.pane().status(),
            Status::Ready,
            "the reconnect started dirty"
        );
    }

    #[test]
    fn connected_does_not_cancel_a_turn_that_is_already_in_flight() {
        // `Connected` is queued like any other update, so it can be
        // processed after the user has already sent something -- type fast
        // enough at startup, or have an adapter slow to connect, and the
        // ordering inverts. Found by driving the real event loop headlessly:
        // a wedged adapter went undetected because this had quietly marked
        // its turn idle.
        let mut app = App::new("main", "b0");
        app.panes.push(Pane::new("fork-1", "b1"));
        app.panes[0].sent_request();
        // pane 1 is already Connecting

        app.apply_transport(Transport::Connected);

        assert!(
            app.panes[0].status() == Status::Waiting,
            "a turn in flight was marked idle by the transport coming up"
        );
        // And the counter-assertion, because "leaves everything alone" would
        // satisfy the check above just as well and would break the thing
        // Connected is actually for.
        assert!(
            app.panes[1].status() == Status::Ready,
            "a pane that was not mid-turn should have been promoted"
        );
    }

    #[test]
    fn connected_still_recovers_a_pane_that_had_been_disconnected() {
        // The reconnect case: a pane left `Gone` by a dropped transport has
        // to come back when the transport does, or the session is dead on
        // screen while working underneath.
        let mut app = App::new("main", "b0");
        app.apply_transport(Transport::Disconnected("gone".to_owned()));
        assert_eq!(app.pane().status(), Status::Gone);

        app.apply_transport(Transport::Connected);
        assert!(
            app.pane().status() == Status::Ready,
            "a recovered pane stayed Gone"
        );
    }

    #[test]
    fn disconnected_reaches_every_pane_and_the_user_can_see_it() {
        // The consequence that actually matters: `render_input`'s
        // `Status::Gone` branch is the only place the user is told the
        // socket died. If Disconnected never reaches a pane, that branch can
        // never fire and anything typed goes into a dead channel silently.
        let mut app = App::new("main", "b0");
        app.panes.push(Pane::new("fork-1", "b1"));

        app.apply_transport(Transport::Disconnected(
            "server closed the connection".to_owned(),
        ));

        for p in &app.panes {
            assert!(
                p.status() == Status::Gone
                    && p.gone_reason() == Some("server closed the connection"),
                "every pane should show the drop, not just the one whose lane happened to match"
            );
        }
    }

    /// `replay()` is what the model is told the conversation was, whenever
    /// there is no `previous_response_id` -- which is every rewind and every
    /// fork. It had no tests at all: one production caller and nothing
    /// asserting any of it.
    ///
    /// A defect here does not crash and does not garble the screen. It
    /// silently changes what the model believes was said, on exactly the
    /// paths a user reaches when they are already trying to correct
    /// something.
    #[test]
    fn replay_is_every_non_system_entry_in_order() {
        let mut app = App::new("main", "b0");
        app.push(Who::User, "first");
        app.push(Who::Model, "a reply");
        app.push(Who::User, "second");

        // Order and text together. A replay that reversed the conversation,
        // or dropped its middle, is as wrong as an empty one -- and a length
        // check alone cannot see either.
        assert_eq!(
            app.pane().replay(),
            vec![
                ("user".to_owned(), "first".to_owned()),
                ("assistant".to_owned(), "a reply".to_owned()),
                ("user".to_owned(), "second".to_owned()),
            ]
        );
    }

    #[test]
    fn replay_omits_system_notices_and_keeps_what_surrounds_them() {
        // System entries are Kobold's own words -- "interrupted", a failed
        // turn, an engine notice. Replaying one would tell the model it had
        // said something it never said.
        //
        // The partner assertion is the surrounding pair: a filter inverted to
        // drop everything would satisfy "no system text" perfectly.
        let mut app = App::new("main", "b0");
        app.push(Who::User, "before");
        app.push(Who::System, "interrupted");
        app.push(Who::Model, "after");

        let got = app.pane().replay();
        assert!(
            !got.iter().any(|(_, text)| text == "interrupted"),
            "a system notice reached the model as something it said: {got:?}"
        );
        assert_eq!(got.len(), 2, "the entries around it must survive: {got:?}");
    }

    #[test]
    fn replay_calls_the_model_assistant_and_only_the_user_user() {
        // The role strings are not cosmetic. An assistant turn replayed as a
        // user turn is a different conversation; replayed with the wrong
        // content-part kind the server rejects the request and drops the
        // whole connection, which is the subtle half of the
        // replay path.
        let mut app = App::new("main", "b0");
        app.push(Who::Model, "mine");
        app.push(Who::User, "yours");

        let got = app.pane().replay();
        assert_eq!(
            got[0].0, "assistant",
            "the model must not be replayed as the user"
        );
        assert_eq!(got[1].0, "user");
        // And nothing else is ever a third role: the wire has two.
        assert!(
            got.iter().all(|(r, _)| r == "user" || r == "assistant"),
            "{got:?}"
        );
    }

    #[test]
    fn an_empty_transcript_replays_as_nothing_rather_than_as_one_blank_turn() {
        // A replay of `vec![("user", "")]` would open every fresh fork with a
        // blank user message, which reads to the model as the user having
        // said nothing at all and is billed for.
        let app = App::new("main", "b0");
        assert!(app.pane().replay().is_empty());
    }

    #[test]
    fn replay_preserves_text_exactly_including_newlines_and_quotes() {
        // It feeds a JSON request. Text that survives the screen but not the
        // wire is the failure this guards, and it is invisible to a fixture
        // of plain words.
        let mut app = App::new("main", "b0");
        let nasty = "he said \"no\"\nthen left\ttabbed";
        app.push(Who::User, nasty);
        assert_eq!(
            app.pane().replay(),
            vec![("user".to_owned(), nasty.to_owned())]
        );
    }

    /// `replay()` over an arbitrary transcript, rather than the four
    /// hand-built ones above.
    ///
    /// The unit tests pin the cases someone thought of. This pins the
    /// relationship: whatever sequence of entries a session produces, what
    /// the model is told must be exactly the non-system ones, in order, with
    /// their text intact. An off-by-one, a filter that drops the last entry,
    /// or a role mapped from the wrong side all violate it and none of them
    /// need an exotic input to appear.
    ///
    /// Generated rather than enumerated because the state space is sequences:
    /// exhausting three entries proves nothing about eleven, and the ordering
    /// bugs live in the longer ones.
    #[test]
    fn replay_is_the_non_system_entries_in_order_whatever_the_session_did() {
        use proptest::prelude::*;

        // 0 = User, 1 = Model, 2 = System. A concrete small alphabet rather
        // than an `Arbitrary` impl on `Who`: the property is about ordering
        // and filtering, and deriving a generator on a UI enum would put a
        // test-only trait on a production type.
        let entries = proptest::collection::vec((0u8..3, r"\PC{0,24}"), 0..12);

        proptest!(|(entries in entries)| {
            let mut app = App::new("main", "b0");
            for (kind, text) in &entries {
                let who = match kind { 0 => Who::User, 1 => Who::Model, _ => Who::System };
                app.push(who, text.clone());
            }

            let want: Vec<(String, String)> = entries
                .iter()
                .filter(|(k, _)| *k != 2)
                .map(|(k, t)| {
                    ((if *k == 0 { "user" } else { "assistant" }).to_owned(), t.clone())
                })
                .collect();

            prop_assert_eq!(app.pane().replay(), want);
        });
    }

    // ---- Resilience at the update boundary -------------------------------
    //
    // An adapter is another process, and after phase 2 it may be one a
    // stranger wrote. Everything below is a frame a correct adapter would
    // never send, asserted to be survivable rather than merely unlikely.

    #[test]
    fn an_update_for_a_lane_with_no_pane_is_dropped_rather_than_misrouted() {
        // Reachable without malice: a pane closed while its turn was in
        // flight. The danger is not the drop -- it is delivering another
        // pane's text into whichever pane happens to be first.
        let mut app = App::new("main", "b0");
        app.push(Who::User, "mine");
        let before = app.pane().transcript.len();

        app.apply("fork-9", content("m1", "not yours"));

        assert_eq!(
            app.pane().transcript.len(),
            before,
            "a foreign lane wrote into this pane"
        );
        assert!(
            !app.pane()
                .transcript
                .iter()
                .any(|e| e.text.contains("not yours")),
            "text for an unknown lane reached a pane"
        );
    }

    #[test]
    fn a_terminal_update_repeated_does_not_settle_a_request_twice() {
        // The invariant `outstanding` rests on: one terminal update per
        // request. A duplicate must not push the count below zero, because a
        // pane that reads Ready with a turn genuinely in flight is the
        // four-bug shape this codebase spent two days removing.
        let mut app = App::new("main", "b0");
        app.sent_request("main");
        assert!(
            app.pane().status() == Status::Waiting,
            "the request should be outstanding"
        );

        app.apply("main", finished_costing(0));
        app.apply("main", finished_costing(0));
        app.apply("main", finished_costing(0));

        // Still idle rather than panicking or wrapping to a huge count. The
        // saturating decrement is what makes a repeat harmless, and nothing
        // asserted it before.
        assert!(
            app.pane().status() != Status::Waiting,
            "a settled request came back"
        );

        // And the pane still works afterwards: a duplicate must not leave the
        // count in a state where the next real request cannot be tracked.
        app.sent_request("main");
        assert!(
            app.pane().status() == Status::Waiting,
            "the pane stopped tracking after a repeat"
        );
    }

    #[test]
    fn an_enormous_delta_is_appended_rather_than_refused_or_truncated() {
        // A tool result can legitimately be a whole file, and the model can
        // legitimately quote it back. The budget for arrival rate lives in
        // the adapter's bounded channel; there is deliberately no cap here,
        // and this pins that so nobody adds a silent one -- a truncated reply
        // that looks complete is worse than a slow one.
        let mut app = App::new("main", "b0");
        let huge = "x".repeat(200_000);
        app.apply(
            "main",
            Incoming::TextMessageStart {
                base: Default::default(),
                message_id: "m1".to_owned(),
                role: None,
            },
        );
        app.apply("main", content("m1", &huge));

        let got = &app
            .pane()
            .transcript
            .last()
            .expect("a delta made an entry")
            .text;
        assert_eq!(
            got.len(),
            huge.len(),
            "a large delta was truncated on the way in"
        );
    }

    /// The derived-status invariant, over arbitrary sequences of the four
    /// things that move it.
    ///
    /// **Four separate bugs violated this before it was derived rather than
    /// set**, each one a pane reading `Ready` while a turn was genuinely in
    /// flight, and each one silent: a queued message dispatches onto a lane
    /// that is still busy, and the adapter-silence watch goes blind because
    /// it keys off the same flag.
    ///
    /// Generated rather than enumerated because the failures were about
    /// *sequences* -- a `Connected` arriving mid-turn, a tool result racing
    /// its own completion. Three-step cases were all anyone thought to write,
    /// and the state space is combinatorial in length.
    #[test]
    fn a_pane_waits_exactly_when_it_has_something_outstanding() {
        use proptest::prelude::*;

        // 0 send, 1 settle, 2 abandon (interrupt), 3 connect, 4 disconnect.
        let steps = proptest::collection::vec(0u8..5, 0..40);

        proptest!(|(steps in steps)| {
            let mut pane = Pane::new("main", "b0");
            // Tracked alongside, saturating exactly as the real one does, so
            // the property is about the *relationship* rather than restating
            // the implementation.
            let mut expected: usize = 0;
            let mut gone = false;

            for step in &steps {
                match step {
                    0 => { pane.sent_request(); expected += 1; }
                    1 => { pane.request_settled(); expected = expected.saturating_sub(1); }
                    2 => { pane.abandon_requests(); expected = 0; }
                    3 => { pane.link = Link::Up; gone = false; }
                    _ => { pane.abandon_requests(); pane.link = Link::Gone("x".into());
                           expected = 0; gone = true; }
                }

                let status = pane.status();
                if gone {
                    // A dead transport is the more important fact.
                    prop_assert_eq!(status, Status::Gone);
                } else if expected > 0 {
                    prop_assert_eq!(
                        status, Status::Waiting,
                        "{} outstanding but the pane read idle", expected
                    );
                } else {
                    prop_assert_ne!(
                        status, Status::Waiting,
                        "nothing outstanding but the pane read busy"
                    );
                }
            }

            // And the count never wrapped: a settle without a send must
            // saturate, or one stray terminal update makes a pane wait
            // forever on a request that does not exist.
            prop_assert!(pane.outstanding < usize::MAX / 2, "outstanding underflowed");
        });
    }

    #[test]
    fn the_caret_lands_where_the_prompt_actually_is() {
        // `layout_input` is tested for the cell offset; nothing asserted what
        // `render_input` does with it. That is the content-versus-geometry
        // split in one place: the prompt renders correctly whether the caret
        // is two columns left, at the origin, or off the end of the line, and
        // every text assertion in this file passes for all three.
        //
        // The two-column offset is the marker plus its space -- "› hello" --
        // so the caret sits on the `l`, not on the `›`.
        let mut app = App::new("main", "b0");
        app.pane_mut().set_input("hello".into());
        app.pane_mut().home();
        for _ in 0..3 {
            app.pane_mut().move_right(false);
        }

        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 3));
        let at = app
            .render_input(&mut buf, Rect::new(0, 0, 40, 3))
            .expect("a caret");
        assert_eq!(
            at.x,
            3 + 2,
            "caret should be past the marker, its space, and three chars"
        );
        assert_eq!(
            at.y, 0,
            "a single-row prompt puts the caret on the first row"
        );

        // Offset area: the caret is relative to where the prompt was drawn,
        // not to the screen origin. A `+` that became a `*` gives 0 here and
        // is invisible at the origin, which is where every other test draws.
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 8));
        let at = app
            .render_input(&mut buf, Rect::new(4, 2, 30, 3))
            .expect("a caret");
        assert_eq!(at.x, 4 + 2 + 3, "caret ignored the area's x");
        assert_eq!(at.y, 2, "caret ignored the area's y");
    }

    #[test]
    fn the_caret_follows_the_prompt_onto_its_second_row() {
        // A wrapped prompt is the case where the row arithmetic matters: the
        // caret must move down with the text rather than staying on the first
        // line while the characters it points at are on the second.
        let mut app = App::new("main", "b0");
        // Wider than the 12-column area below, so it wraps.
        app.pane_mut().set_input("aaaaaaaa bbbbbbbb".into());

        let area = Rect::new(0, 0, 12, 4);
        let mut buf = Buffer::empty(Rect::new(0, 0, 12, 8));
        let at = app.render_input(&mut buf, area).expect("a caret");
        let rows = app.pane().layout_input(area.width as usize - 2).0;
        assert!(rows.len() > 1, "the fixture must actually wrap: {rows:?}");
        assert_eq!(
            at.y as usize,
            area.y as usize + rows.len() - 1,
            "the caret is at the end of the input, so it belongs on the last row"
        );
    }

    #[test]
    fn a_prompt_taller_than_its_area_keeps_the_caret_on_screen() {
        // Reachable by pasting or by typing a long message: the prompt is
        // capped at eight rows, so a longer input scrolls within itself and
        // the caret's row must be measured from the first VISIBLE row rather
        // than from the first row of the text.
        //
        // The case the other caret tests cannot reach, because they never
        // scroll -- and the arithmetic that is wrong here puts the caret off
        // the bottom of the prompt while the text it points at is on screen.
        let mut app = App::new("main", "b0");
        let long: String = (0..12)
            .map(|i| format!("line{i} padding padding "))
            .collect::<Vec<_>>()
            .join("");
        app.pane_mut().set_input(long);

        let area = Rect::new(0, 1, 16, 3);
        let mut buf = Buffer::empty(Rect::new(0, 0, 16, 10));
        let at = app.render_input(&mut buf, area).expect("a caret");

        let rows = app.pane().layout_input(area.width as usize - 2).0;
        assert!(
            rows.len() > area.height as usize,
            "the fixture must overflow its area: {} rows in {}",
            rows.len(),
            area.height
        );
        assert!(
            at.y >= area.y && at.y < area.bottom(),
            "caret at row {} is outside the prompt's own rows {}..{}",
            at.y,
            area.y,
            area.bottom()
        );
        // The caret is at the end of the input, so it belongs on the last
        // visible row rather than anywhere else inside the window.
        assert_eq!(
            at.y,
            area.bottom() - 1,
            "a caret at the end should sit on the last row shown"
        );
    }

    #[test]
    fn the_prompt_marker_is_on_the_first_row_and_only_the_first() {
        // The marker says whether the session is ready, connecting or gone,
        // and it belongs at the start of the prompt. Inverted, every row
        // except the first carries it -- which on a wrapped prompt draws a
        // column of markers down the side and leaves the row that should
        // have one blank.
        let mut app = App::new("main", "b0");
        app.pane_mut().set_input("aaaaaaaa bbbbbbbb".into());
        // Disconnected on purpose: a ready marker is drawn in the same dim
        // colour as a continuation row, so the ready state cannot tell the
        // two styles apart. Gone is red, and it is also the state where the
        // marker matters most -- it is the only thing on screen saying the
        // session is dead.
        app.apply_transport(Transport::Disconnected("gone".into()));

        let area = Rect::new(0, 0, 12, 4);
        let mut buf = Buffer::empty(Rect::new(0, 0, 12, 8));
        app.render_input(&mut buf, area);

        let cell = |x: u16, y: u16| buf[(x, y)].symbol().to_owned();
        assert_eq!(cell(0, 0), "✕", "the first row should carry the marker");
        assert_eq!(cell(0, 1), " ", "a continuation row should not");

        // And its colour, separately. The marker's whole job is to be
        // noticed -- ready, connecting, or gone -- so drawing it in the dim
        // continuation style says the opposite of what it means, and the
        // symbol assertion above cannot see that at all.
        // Foreground only: the buffer fills in a background and an underline
        // colour of its own, so comparing whole styles compares those too.
        let fg = |x: u16, y: u16| buf[(x, y)].style().fg;
        assert_eq!(
            fg(0, 0),
            Some(Color::Red),
            "a dead session's marker was drawn in the dim continuation colour"
        );
        assert_eq!(
            fg(0, 1),
            Some(Color::DarkGray),
            "a continuation row was drawn as if it were the marker"
        );
    }

    /// `cy - top` to `cy + top` in `render_input` is **equivalent**, proved
    /// rather than left as a survivor.
    ///
    /// `top` is `cy.saturating_sub(height - 1)`. So either `cy` is below
    /// `height - 1`, in which case `top` is 0 and the two agree exactly, or
    /// it is not, in which case `cy - top` is `height - 1` while `cy + top`
    /// is at least that and the `y.min(area.bottom() - 1)` clamp on the next
    /// line brings it back to the same row.
    ///
    /// Recorded as a test rather than a comment so the reasoning is checked:
    /// if `top` is ever redefined, or the clamp removed, this stops holding
    /// and the mutant stops being equivalent.
    #[test]
    fn the_prompts_scroll_offset_is_defined_so_the_caret_cannot_leave_the_box() {
        for height in 1u16..9 {
            for cy in 0usize..20 {
                let top = cy.saturating_sub(height.saturating_sub(1) as usize);
                let correct = cy - top;
                let mutated = cy + top;
                let clamp = height.saturating_sub(1) as usize;
                assert_eq!(
                    correct.min(clamp),
                    mutated.min(clamp),
                    "height {height}, caret row {cy}: the clamp no longer absorbs the difference, \
                     so `cy + top` has become observable and needs a real test"
                );
            }
        }
    }

    #[test]
    fn the_menu_highlights_the_row_the_arrows_are_on_and_no_other() {
        // The suggestion list's only state is which row is chosen, and every
        // existing test asserts the NAMES it shows. A highlight on the wrong
        // row -- or on all of them -- renders identical text, so none of them
        // can see it, and the user presses Tab and gets a command they were
        // not pointing at.
        let mut app = App::new("main", "b0");
        app.pane_mut().set_input("/".into());
        let n = app.menu_rows();
        assert!(n >= 4, "the fixture needs several rows to tell them apart");

        let area = Rect::new(0, 0, 40, n);
        let bg_of = |app: &App, row: u16| {
            let mut buf = Buffer::empty(Rect::new(0, 0, 40, n + 2));
            app.render_menu(&mut buf, area);
            buf[(1, row)].style().bg
        };

        let lit = |app: &App| {
            (0..n)
                .filter(|r| bg_of(app, *r) == Some(Color::Indexed(238)))
                .collect::<Vec<_>>()
        };

        assert_eq!(
            lit(&app),
            vec![0],
            "the first row should be chosen to begin with"
        );

        for i in 1..n {
            app.menu_move(1);
            assert_eq!(lit(&app), vec![i], "the highlight did not follow the arrow");
        }
    }

    #[test]
    fn the_panel_highlights_the_row_the_caret_is_on_and_no_other() {
        // Same shape as the menu one band down, and it matters more: the
        // panel's rows are answers, so a highlight on the wrong row means
        // Enter submits an option the user was not looking at.
        //
        // The highlight is tied to the option's own TEXT rather than to a row
        // number, because the panel draws its question above the options and
        // a row index is then an assertion about layout rather than about
        // which answer is selected. An earlier version indexed rows and
        // passed with the comparison inverted.
        let mut app = App::new("main", "b0");
        app.park_ask(
            "main",
            crate::tools::Ask {
                call_id: "c1".to_owned(),
                question: "which?".to_owned(),
                options: vec!["alpha".to_owned(), "beta".to_owned()],
                multiple: false,
            },
        );

        let rows = app.panel_rows();
        let area = Rect::new(0, 0, 40, rows);

        // Which rows carry the highlight, and what text is on them.
        let lit_text = |app: &App| {
            let mut buf = Buffer::empty(Rect::new(0, 0, 40, rows + 4));
            app.render_panel(&mut buf, area);
            (0..rows)
                .filter(|r| {
                    (0..40u16).any(|x| buf[(x, *r)].style().bg == Some(Color::Indexed(238)))
                })
                .map(|r| {
                    (0..40u16)
                        .map(|x| buf[(x, r)].symbol().to_owned())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
        };

        let first = lit_text(&app);
        assert_eq!(
            first.len(),
            1,
            "exactly one row should be highlighted: {first:?}"
        );
        assert!(
            first[0].contains("alpha"),
            "the caret starts on the first option: {first:?}"
        );
        assert!(
            !first[0].contains("beta"),
            "two options were highlighted at once: {first:?}"
        );

        app.panel_move(1);
        let second = lit_text(&app);
        assert_eq!(second.len(), 1, "still exactly one: {second:?}");
        assert!(
            second[0].contains("beta"),
            "the highlight did not move to the next option: {second:?}"
        );
        assert!(
            !second[0].contains("alpha"),
            "the old row stayed lit: {second:?}"
        );
    }
}
