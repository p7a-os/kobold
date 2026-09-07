//! Tools the model can call, and the registry that resolves a call to one.
//!
//! Two kinds share this interface. Local tools run in this process and are
//! defined below. Tools reached over MCP register the same way, so the model is
//! offered one list and never has to know which side of the boundary a tool
//! lives on.
//!
//! Server-side tools -- the ones the API runs itself, like web search -- do not
//! appear here at all: they are named in the request and executed remotely, and
//! nothing ever comes back for us to run.

use std::path::{Path, PathBuf};

/// A call as the model asked for it.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    /// Ties the result back to the request. The API rejects a
    /// `function_call_output` whose `call_id` does not match one it issued.
    pub id: String,
    pub name: String,
    /// Raw JSON, unparsed: each tool knows its own arguments, and a shared
    /// pre-parse would have to guess a shape that fits all of them.
    pub arguments: String,
}

/// What running a tool produced.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Finished. This text goes back as the call's output.
    Done(String),
    /// Needs the person at the keyboard. The turn parks until they answer, and
    /// the answer becomes the output.
    Ask(Ask),
    /// The call was wrong in a way the model can correct -- unknown tool, bad
    /// arguments, a path outside the cone. Returned to the model as the tool's
    /// output rather than raised, so it can try again instead of the turn
    /// dying.
    Refused(String),
}

/// A question to put to the user, with a fixed set of answers and always the
/// option of writing one instead.
#[derive(Debug, Clone, PartialEq)]
pub struct Ask {
    pub call_id: String,
    pub question: String,
    /// At most `MAX_OPTIONS`. More than that stops being a panel and starts
    /// being a menu, and the model is asked to narrow it rather than the UI
    /// growing to fit.
    pub options: Vec<String>,
    /// Checkbox rather than radio: several answers may be chosen.
    pub multiple: bool,
}

/// Options a single `ask` may offer, before the free-text one.
pub const MAX_OPTIONS: usize = 4;

/// The largest file `file_read` will return, before truncating.
///
/// A tool result is spent from the same context window as the conversation, so
/// an unbounded read does not fail loudly -- it quietly evicts the discussion
/// that motivated it.
pub const MAX_READ_BYTES: usize = 64 * 1024;

/// Resolve `requested` against `root`, refusing anything that lands outside.
///
/// "Inside" is decided after the filesystem has had its say, not before: the
/// path is canonicalised, so `..` is collapsed and symlinks are followed, and
/// only the real destination is compared. Checking the string first and
/// canonicalising later is the classic way to be talked out of a sandbox --
/// `a/../../etc/passwd` contains no leading `..`, and a symlink contains no
/// `..` at all.
///
/// Comparison is by path component. A prefix test on the string would let
/// `/home/user-elsewhere` pass a `/home/user` cone.
pub fn resolve_in_cone(root: &Path, requested: &str) -> Result<PathBuf, String> {
    if requested.trim().is_empty() {
        return Err("no path given".to_owned());
    }
    let root = root
        .canonicalize()
        .map_err(|e| format!("cannot resolve the working directory: {e}"))?;
    let joined = {
        let p = Path::new(requested);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            root.join(p)
        }
    };
    // Scope is settled without ever asking whether the file itself is there.
    // Canonicalising the whole path would answer that along the way -- it fails
    // with "no such file" for a missing leaf and succeeds for a present one, so
    // `../../etc/shadow` and `../../etc/nothing-here` would come back
    // differently and the refusal would become a way to probe outside the cone.
    if !resolve_ancestors(&joined).starts_with(&root) {
        return Err(outside(requested));
    }
    // Then the filesystem, for the part that reasoning about the string cannot
    // settle: a symlink names nothing about its target, so only following it
    // says where it goes.
    let real = joined
        .canonicalize()
        .map_err(|e| format!("cannot read '{requested}': {e}"))?;
    if !real.starts_with(&root) {
        return Err(outside(requested));
    }
    Ok(real)
}

/// One wording for every way out of the cone, naming only what the caller
/// already typed. Anything more specific is a description of the filesystem it
/// exists to keep out of reach.
fn outside(requested: &str) -> String {
    format!("'{requested}' is outside the working directory")
}

/// Settle a path's spelling as far as the filesystem already knows it, and
/// reason about the rest.
///
/// Reasoning about the string alone is not enough, and macOS is where that
/// shows: `/var` is a symlink to `/private/var`, so a temporary directory is
/// spelled `/var/folders/...` while the canonical root it lives under is
/// `/private/var/folders/...`. Compared as text those do not match, and a file
/// plainly inside the cone was refused as outside. Linux has no such symlink on
/// `/tmp`, which is why it passed here and failed there.
///
/// So the deepest ancestor that exists is canonicalised, which resolves any
/// symlinked prefix, and whatever does not exist yet is appended and reduced
/// lexically. The leaf is never asked about, which is what keeps the refusal
/// from reporting whether it is there.
///
/// This is a filter, not the verdict: `canonicalize` still runs afterwards and
/// still has the last word, so being generous here can only cost a second
/// check, never let something out.
fn resolve_ancestors(p: &Path) -> PathBuf {
    // Collapsed first, so `file_name` is meaningful at every step -- a path
    // ending in `..` has none, and the walk below would stop early.
    let lexical = normalise(p);
    let mut trailing: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = lexical.clone();
    loop {
        if let Ok(real) = cur.canonicalize() {
            let mut out = real;
            for part in trailing.iter().rev() {
                out.push(part);
            }
            return normalise(&out);
        }
        let (Some(name), Some(parent)) = (cur.file_name(), cur.parent()) else {
            // Walked to a root that will not canonicalise. Nothing left to
            // resolve, so the lexical answer is the best one available.
            return lexical;
        };
        trailing.push(name.to_owned());
        cur = parent.to_path_buf();
    }
}

/// Resolve `.` and `..` by reasoning about the path rather than by asking the
/// filesystem. Symlinks are deliberately not followed here -- that is the next
/// step's job, and doing it here would reintroduce the question this avoids.
fn normalise(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            // Popping past the start cannot escape: `starts_with` on the
            // result then fails against any absolute root, which is the
            // refusal we want.
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Read a file, refusing anything outside the working directory.
pub fn file_read(root: &Path, arguments: &str) -> Outcome {
    #[derive(serde::Deserialize)]
    struct Args {
        path: String,
    }
    let args: Args = match crate::json::from_slice(arguments.as_bytes()) {
        Ok(a) => a,
        Err(e) => return Outcome::Refused(format!("bad arguments: {e}")),
    };
    let path = match resolve_in_cone(root, &args.path) {
        Ok(p) => p,
        Err(e) => return Outcome::Refused(e),
    };
    // Refused rather than read: a directory read would otherwise surface as an
    // obscure IO error, and the model can act on being told which it was.
    if path.is_dir() {
        return Outcome::Refused(format!("'{}' is a directory", args.path));
    }
    match std::fs::read(&path) {
        Err(e) => Outcome::Refused(format!("cannot read '{}': {e}", args.path)),
        Ok(bytes) => {
            let total = bytes.len();
            let clipped = &bytes[..total.min(MAX_READ_BYTES)];
            // Lossy on purpose. A file that is nearly text should come back
            // readable rather than as an error, and a truncation in the middle
            // of a multi-byte character is not worth failing over.
            let mut text = String::from_utf8_lossy(clipped).into_owned();
            if total > MAX_READ_BYTES {
                text.push_str(&format!(
                    "\n\n[truncated: {total} bytes total, first {MAX_READ_BYTES} shown]"
                ));
            }
            Outcome::Done(text)
        }
    }
}

/// Parse an `ask` call into the question to put on screen.
pub fn ask(call_id: &str, arguments: &str) -> Outcome {
    #[derive(serde::Deserialize)]
    struct Args {
        question: String,
        #[serde(default)]
        options: Vec<String>,
        #[serde(default)]
        multiple: bool,
    }
    let args: Args = match crate::json::from_slice(arguments.as_bytes()) {
        Ok(a) => a,
        Err(e) => return Outcome::Refused(format!("bad arguments: {e}")),
    };
    if args.question.trim().is_empty() {
        return Outcome::Refused("a question is required".to_owned());
    }
    if args.options.len() > MAX_OPTIONS {
        return Outcome::Refused(format!(
            "at most {MAX_OPTIONS} options; {} were given",
            args.options.len()
        ));
    }
    // An option nobody can read is not an option. Dropping them silently would
    // renumber the rest under the model's feet.
    if args.options.iter().any(|o| o.trim().is_empty()) {
        return Outcome::Refused("options cannot be blank".to_owned());
    }
    Outcome::Ask(Ask {
        call_id: call_id.to_owned(),
        question: args.question,
        options: args.options,
        multiple: args.multiple,
    })
}

/// Several MCP servers behind one source.
///
/// A session may connect to more than one, and the model is shown a single
/// list, so something has to decide which server a name belongs to. Names are
/// namespaced by server at registration, so the question is answerable without
/// asking every server in turn -- the first that claims a name owns it, and no
/// two can claim the same one.
#[derive(Default)]
pub struct Sources(Vec<Box<dyn McpTools>>);

impl Sources {
    pub fn new() -> Self {
        Sources(Vec::new())
    }

    pub fn push(&mut self, source: Box<dyn McpTools>) {
        self.0.push(source);
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Everything every server offers, for telling the model what exists.
    pub fn all_schemas(&self) -> Vec<(String, String, String)> {
        self.0.iter().flat_map(|s| s.schemas()).collect()
    }
}

#[async_trait::async_trait]
impl McpTools for Sources {
    fn owns(&self, name: &str) -> bool {
        self.0.iter().any(|s| s.owns(name))
    }

    async fn call(&self, call: &Call) -> Outcome {
        match self.0.iter().find(|s| s.owns(&call.name)) {
            Some(source) => source.call(call).await,
            // Reachable only if `owns` and this disagree, which would mean a
            // source changed its mind between the two calls. Refused rather
            // than panicking: it is still just a name the model got wrong.
            None => Outcome::Refused(format!("no server offers '{}'", call.name)),
        }
    }

    fn schemas(&self) -> Vec<(String, String, String)> {
        self.all_schemas()
    }
}

/// What the event loop should do about a call.
///
/// Separated from running it so the decision can be tested without a socket, a
/// terminal or a running turn. The loop does the IO; this says what the IO
/// should be.
#[derive(Debug, Clone, PartialEq)]
pub enum Dispatch {
    /// Send this straight back as the call's output.
    Reply {
        output: String,
        /// Whether the call failed rather than answered.
        ///
        /// **`Done` and `Refused` used to collapse into the same reply**, so
        /// an unknown tool, bad arguments and a path outside the cone all
        /// reached the model as ordinary output and it had to infer failure
        /// from prose. AG-UI's `ToolMessage` has an `error` field for exactly
        /// this, and the specification is direct about why: without it a tool
        /// that failed is indistinguishable from one that succeeded.
        error: bool,
    },
    /// Put this question on screen. The turn stays open until it is answered.
    Park(Ask),
}

/// What happens to an outcome.
///
/// Deliberately separate from running the call, and deliberately pure. Once
/// tools can live on the far side of a network, folding this into execution
/// would mean it could only be tested with a server running -- and code that
/// cannot be tested cheaply is code whose mutants cannot be caught.
pub fn decide(outcome: Outcome) -> Dispatch {
    match outcome {
        Outcome::Done(text) => Dispatch::Reply {
            output: text,
            error: false,
        },
        // Still a reply rather than a raised error: the model can correct a
        // refusal and try again, where a dead turn just ends. What changes is
        // that it is now *labelled* one instead of being left to read as
        // ordinary output.
        Outcome::Refused(text) => Dispatch::Reply {
            output: text,
            error: true,
        },
        // Parked whether or not a question is already showing. A second one is
        // added to the panel rather than refused: the API allows a model to ask
        // two things in one turn, and it does. Refusing the second turned one
        // form into a conversation -- the model asked, was told to wait, and
        // asked again once the first was answered.
        Outcome::Ask(ask) => Dispatch::Park(ask),
    }
}

/// Tools reached over MCP, as the rest of the program needs to see them.
///
/// Three methods, and small on purpose: this is the shape a test fakes, so
/// every method has to be one a fake can plausibly answer. The MCP client
/// implements it; nothing above this line knows a server exists.
#[async_trait::async_trait]
pub trait McpTools: Send + Sync {
    /// Whether this source owns the name the model called. Names are
    /// namespaced by server, so this is a prefix question.
    fn owns(&self, name: &str) -> bool;
    /// Run it. Errors from the server become `Refused`, not a dead turn.
    async fn call(&self, call: &Call) -> Outcome;
    /// What to tell the model exists, as (name, description, JSON schema).
    fn schemas(&self) -> Vec<(String, String, String)>;
}

/// The JSON Schema for each local tool, as the API wants to be told about it.
///
/// Written out rather than derived from the argument structs. A schema is a
/// prompt: the descriptions are what the model reads to decide whether a tool
/// applies and what to put in it, and deriving them would produce something
/// correct and useless. The cone and the option limit are stated here too,
/// because a model that knows the rule asks in bounds, while one that finds out
/// by being refused spends a turn discovering it.
pub fn schemas() -> Vec<(&'static str, &'static str, String)> {
    vec![
        (
            "file_read",
            "Read a UTF-8 text file from the working directory. Paths may be \
             relative to it or absolute, but must resolve inside it: anything \
             outside, including by way of `..` or a symbolic link, is refused. \
             Large files come back truncated with a note saying so.",
            r#"{
              "type": "object",
              "properties": {
                "path": {
                  "type": "string",
                  "description": "Path to the file, inside the working directory."
                }
              },
              "required": ["path"],
              "additionalProperties": false
            }"#
            .to_owned(),
        ),
        (
            "ask",
            "Put a question to the person at the keyboard and wait for their \
             answer. Offer up to four options; they can always write something \
             else instead, so the options are shortcuts rather than the only \
             answers. Use it when a choice is genuinely theirs to make, not to \
             confirm something already agreed.",
            format!(
                r#"{{
                  "type": "object",
                  "properties": {{
                    "question": {{
                      "type": "string",
                      "description": "The question, as one short sentence."
                    }},
                    "options": {{
                      "type": "array",
                      "items": {{ "type": "string" }},
                      "maxItems": {MAX_OPTIONS},
                      "description": "Up to {MAX_OPTIONS} suggested answers. May be empty."
                    }},
                    "multiple": {{
                      "type": "boolean",
                      "description": "True to let several options be chosen at once."
                    }}
                  }},
                  "required": ["question"],
                  "additionalProperties": false
                }}"#
            ),
        ),
    ]
}

/// Every local tool, by the name the model calls it by.
pub const LOCAL: &[&str] = &["file_read", "ask"];

/// Run a tool, local or remote. `root` is the working directory the cone is
/// measured from.
///
/// Local names are checked first and cannot be taken over: an MCP server that
/// offers `file_read` is namespaced to `server__file_read` at registration, so
/// there is no name it can present that would reach this match instead of ours.
///
/// The local tools do blocking IO inside an async function. That is a
/// deliberate and bounded exception -- a capped read of a local file, measured
/// in microseconds -- rather than a general licence; anything that could block
/// for longer belongs on `spawn_blocking`.
pub async fn run(root: &Path, call: &Call, mcp: Option<&dyn McpTools>) -> Outcome {
    match call.name.as_str() {
        "file_read" => file_read(root, &call.arguments),
        "ask" => ask(&call.id, &call.arguments),
        other => {
            if let Some(source) = mcp.filter(|m| m.owns(other)) {
                return source.call(call).await;
            }
            // Refused rather than an error: the model chose the name, and
            // telling it what exists is more useful than failing the turn.
            let mut known: Vec<String> = LOCAL.iter().map(|n| (*n).to_owned()).collect();
            if let Some(m) = mcp {
                known.extend(m.schemas().into_iter().map(|(name, _, _)| name));
            }
            Outcome::Refused(format!(
                "no tool named '{other}'; available: {}",
                known.join(", ")
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A working directory with a file in it, plus a sibling directory outside
    /// the cone to try to escape into.
    struct Cone {
        _tmp: std::path::PathBuf,
        root: PathBuf,
        outside: PathBuf,
    }

    impl Cone {
        fn new(tag: &str) -> Cone {
            let tmp =
                std::env::temp_dir().join(format!("kobold-cone-{tag}-{}", std::process::id()));
            let root = tmp.join("work");
            let outside = tmp.join("elsewhere");
            let _ = std::fs::remove_dir_all(&tmp);
            std::fs::create_dir_all(root.join("sub")).expect("mkdir");
            std::fs::create_dir_all(&outside).expect("mkdir");
            std::fs::write(root.join("inside.txt"), b"in the cone").expect("write");
            std::fs::write(root.join("sub/deep.txt"), b"deeper").expect("write");
            std::fs::write(outside.join("secret.txt"), b"not yours").expect("write");
            Cone {
                _tmp: tmp,
                root,
                outside,
            }
        }

        fn call(&self, path: &str) -> Outcome {
            // Built by hand rather than through a JSON library: a temporary
            // directory can contain a backslash on some hosts, and the point of
            // these tests is the path, not the encoder.
            let escaped = path.replace('\\', "\\\\").replace('"', "\\\"");
            file_read(&self.root, &format!(r#"{{"path":"{escaped}"}}"#))
        }
    }

    impl Drop for Cone {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self._tmp);
        }
    }

    #[test]
    fn a_file_inside_the_cone_is_read() {
        let c = Cone::new("inside");
        assert_eq!(
            c.call("inside.txt"),
            Outcome::Done("in the cone".to_owned())
        );
        assert_eq!(c.call("sub/deep.txt"), Outcome::Done("deeper".to_owned()));
        // The long way round to the same file is still the same file.
        assert_eq!(
            c.call("sub/../inside.txt"),
            Outcome::Done("in the cone".to_owned())
        );
        // An absolute path that lands inside is fine; the cone is about where
        // it resolves, not how it was written.
        let abs = c.root.join("inside.txt");
        assert_eq!(
            c.call(abs.to_str().expect("utf8")),
            Outcome::Done("in the cone".to_owned())
        );
    }

    #[test]
    fn traversal_out_of_the_cone_is_refused() {
        let c = Cone::new("traverse");
        for probe in [
            "../elsewhere/secret.txt",
            "sub/../../elsewhere/secret.txt",
            "./sub/./../../elsewhere/secret.txt",
            "/etc/passwd",
            "../../../../../../etc/passwd",
        ] {
            match c.call(probe) {
                Outcome::Refused(_) => {}
                other => panic!("{probe:?} escaped the cone: {other:?}"),
            }
        }
    }

    #[test]
    fn a_symlink_pointing_out_of_the_cone_is_refused() {
        // The case a string check cannot see: the path contains no `..` at all
        // and still resolves outside.
        let c = Cone::new("symlink");
        #[cfg(unix)]
        {
            let link = c.root.join("escape.txt");
            std::os::unix::fs::symlink(c.outside.join("secret.txt"), &link).expect("symlink");
            match c.call("escape.txt") {
                Outcome::Refused(_) => {}
                other => panic!("symlink escaped the cone: {other:?}"),
            }
            // And a symlinked directory, which is the same trick one level up.
            let dir = c.root.join("escape-dir");
            std::os::unix::fs::symlink(&c.outside, &dir).expect("symlink");
            match c.call("escape-dir/secret.txt") {
                Outcome::Refused(_) => {}
                other => panic!("symlinked directory escaped the cone: {other:?}"),
            }
        }
    }

    #[test]
    fn a_refusal_echoes_the_request_but_never_where_it_resolved_to() {
        // Echoing what was asked for gives nothing away -- the caller wrote
        // it. Naming where it actually landed would turn every refusal into a
        // probe of the filesystem the cone exists to keep out of reach, so a
        // symlink is the case that separates the two: its name says nothing
        // about its target.
        let c = Cone::new("quiet");
        #[cfg(unix)]
        {
            let link = c.root.join("notes.txt");
            std::os::unix::fs::symlink(c.outside.join("secret.txt"), &link).expect("symlink");
            let Outcome::Refused(why) = c.call("notes.txt") else {
                panic!("should have been refused");
            };
            assert!(
                why.contains("notes.txt"),
                "refusal should name what was asked for: {why}"
            );
            assert!(!why.contains("secret"), "refusal leaked the target: {why}");
            assert!(
                !why.contains("elsewhere"),
                "refusal leaked the target directory: {why}"
            );
            assert!(why.contains("outside"), "refusal should say why: {why}");
        }
    }

    #[test]
    fn an_absolute_path_through_a_symlinked_ancestor_is_still_inside() {
        // The macOS bug, reproduced where there is no `/var`. On macOS `/var`
        // is a symlink to `/private/var`, so a temporary directory is spelled
        // `/var/folders/...` while its canonical root is `/private/var/...`.
        // Compared as text those disagree and a file plainly inside the cone
        // was refused as outside. Linux has no such symlink on `/tmp`, which is
        // exactly why this passed here and failed there -- so the condition is
        // built rather than waited for.
        #[cfg(unix)]
        {
            let c = Cone::new("symlinked-ancestor");
            let alias = c._tmp.join("alias");
            std::os::unix::fs::symlink(&c.root, &alias).expect("symlink");

            // Spelled through the symlink, resolving to the same file.
            let through = alias.join("inside.txt");
            assert_eq!(
                c.call(through.to_str().expect("utf8")),
                Outcome::Done("in the cone".to_owned()),
                "a symlinked spelling of the cone was refused as outside it"
            );

            // And the cone still holds when the symlink leads out of it.
            let out = c._tmp.join("alias-out");
            std::os::unix::fs::symlink(&c.outside, &out).expect("symlink");
            match c.call(out.join("secret.txt").to_str().expect("utf8")) {
                Outcome::Refused(why) => assert!(why.contains("outside"), "{why}"),
                other => panic!("a symlinked path out of the cone was allowed: {other:?}"),
            }
        }
    }

    #[test]
    fn a_path_outside_the_cone_refuses_the_same_way_whether_or_not_it_exists() {
        // Otherwise the refusal is an oracle: ask for a path outside, and the
        // difference between "outside" and "no such file" tells you whether it
        // is there. The cone is meant to keep that unknowable.
        let c = Cone::new("oracle");
        let Outcome::Refused(present) = c.call("../elsewhere/secret.txt") else {
            panic!("should be refused")
        };
        let Outcome::Refused(absent) = c.call("../elsewhere/no-such-file.txt") else {
            panic!("should be refused")
        };
        let strip = |s: &str, name: &str| s.replace(name, "<path>");
        assert_eq!(
            strip(&present, "secret.txt"),
            strip(&absent, "no-such-file.txt"),
            "existence outside the cone is observable through the refusal"
        );
        assert!(present.contains("outside"), "{present}");
    }

    #[test]
    fn a_sibling_directory_sharing_a_prefix_is_not_inside() {
        // `/tmp/x/work-other` starts with `/tmp/x/work` as a string and is a
        // different directory. Comparison has to be by component.
        let c = Cone::new("prefix");
        let sibling = c.root.with_file_name("work-other");
        std::fs::create_dir_all(&sibling).expect("mkdir");
        std::fs::write(sibling.join("secret.txt"), b"not yours").expect("write");
        match c.call(sibling.join("secret.txt").to_str().expect("utf8")) {
            Outcome::Refused(_) => {}
            other => panic!("a prefix-sharing sibling was treated as inside: {other:?}"),
        }
    }

    #[test]
    fn a_directory_and_a_missing_file_are_refused_distinctly() {
        let c = Cone::new("kinds");
        match c.call("sub") {
            Outcome::Refused(why) => assert!(why.contains("directory"), "{why}"),
            other => panic!("a directory should be refused: {other:?}"),
        }
        match c.call("nope.txt") {
            Outcome::Refused(why) => assert!(why.contains("nope.txt"), "{why}"),
            other => panic!("a missing file should be refused: {other:?}"),
        }
        match c.call("") {
            Outcome::Refused(why) => assert!(why.contains("no path"), "{why}"),
            other => panic!("an empty path should be refused: {other:?}"),
        }
    }

    #[test]
    fn a_large_file_is_truncated_and_says_so() {
        // Silence here would spend the context window on a file and leave the
        // model reasoning about a fragment it believes is whole.
        let c = Cone::new("large");
        let big = vec![b'x'; MAX_READ_BYTES * 2];
        std::fs::write(c.root.join("big.txt"), &big).expect("write");
        let Outcome::Done(text) = c.call("big.txt") else {
            panic!("should have been read");
        };
        assert!(text.starts_with(&"x".repeat(100)));
        assert!(
            text.contains("truncated"),
            "no truncation notice: {}",
            &text[text.len() - 80..]
        );
        assert!(
            text.contains(&(MAX_READ_BYTES * 2).to_string()),
            "should give the real size"
        );
    }

    #[test]
    fn truncation_starts_one_byte_past_the_cap_and_not_at_it() {
        // The boundary, because either side of it is a silent wrong answer: a
        // file exactly at the cap that claims to be truncated sends the model
        // hunting for content that is already all there, and one past it that
        // stays quiet hands over a fragment presented as whole.
        let c = Cone::new("boundary");
        let exact = vec![b'x'; MAX_READ_BYTES];
        std::fs::write(c.root.join("exact.txt"), &exact).expect("write");
        let Outcome::Done(text) = c.call("exact.txt") else {
            panic!("should read")
        };
        assert!(
            !text.contains("truncated"),
            "a file exactly at the cap is whole"
        );
        assert_eq!(text.len(), MAX_READ_BYTES);

        let over = vec![b'x'; MAX_READ_BYTES + 1];
        std::fs::write(c.root.join("over.txt"), &over).expect("write");
        let Outcome::Done(text) = c.call("over.txt") else {
            panic!("should read")
        };
        assert!(
            text.contains("truncated"),
            "one byte past the cap is not whole"
        );
    }

    #[test]
    fn the_read_cap_is_the_documented_size() {
        // Pinned deliberately. The number is a claim about how much of a
        // context window one tool result may spend, so moving it is a decision
        // rather than a detail, and this is what makes it one.
        assert_eq!(MAX_READ_BYTES, 65_536, "the cap is 64 KiB");
        assert_eq!(
            MAX_OPTIONS, 4,
            "the panel shows four options and a fifth to type"
        );
    }

    #[test]
    fn bad_arguments_are_refused_rather_than_raised() {
        // The model chose these, so it is the one that can fix them.
        let c = Cone::new("args");
        for bad in ["", "not json", "{}", r#"{"path": 7}"#] {
            match file_read(&c.root, bad) {
                Outcome::Refused(_) => {}
                other => panic!("{bad:?} should be refused: {other:?}"),
            }
        }
    }

    fn call_of(name: &str, arguments: &str) -> Call {
        Call {
            id: "call_1".into(),
            name: name.into(),
            arguments: arguments.into(),
        }
    }

    /// An MCP source that answers from memory. The point of the trait: none of
    /// this needs a server, a socket or a subprocess to exercise.
    struct FakeMcp {
        prefix: &'static str,
        reply: Outcome,
    }

    #[async_trait::async_trait]
    impl McpTools for FakeMcp {
        fn owns(&self, name: &str) -> bool {
            name.starts_with(self.prefix)
        }
        async fn call(&self, _call: &Call) -> Outcome {
            self.reply.clone()
        }
        fn schemas(&self) -> Vec<(String, String, String)> {
            vec![(
                format!("{}search", self.prefix),
                "search".to_owned(),
                "{}".to_owned(),
            )]
        }
    }

    fn block<F: std::future::Future>(f: F) -> F::Output {
        // A current-thread runtime, because these tests are about routing and
        // nothing in them is actually concurrent.
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(f)
    }

    #[test]
    fn a_finished_call_and_a_refused_one_both_come_back_as_output_but_only_one_as_a_failure() {
        // **This test's contract changed deliberately.** It used to say the
        // model cannot tell them apart *and should not*. The first half is
        // still true of the dispatch -- both are output, and only a dead
        // transport ends a turn, because a model can correct a refusal and
        // try again. The second half was wrong: an unknown tool, bad
        // arguments and a path outside the cone all arrived as ordinary
        // output and the model had to infer failure from prose.
        let c = Cone::new("dispatch");
        let ok = block(run(
            &c.root,
            &call_of("file_read", r#"{"path":"inside.txt"}"#),
            None,
        ));
        assert_eq!(
            decide(ok),
            Dispatch::Reply {
                output: "in the cone".to_owned(),
                error: false
            }
        );

        let refused = block(run(
            &c.root,
            &call_of("file_read", r#"{"path":"../nope"}"#),
            None,
        ));
        let Dispatch::Reply { output, error } = decide(refused) else {
            panic!("a refusal is still output, not a dead turn")
        };
        assert!(output.contains("outside"), "{output}");
        assert!(
            error,
            "a refusal reached the model indistinguishable from an answer"
        );
    }

    #[test]
    fn a_question_always_parks() {
        // A model may ask several things in one turn -- the API allows it and
        // models do it. Refusing a second question because the panel is
        // already showing one turned a single form into a conversation:
        // asked, told to wait, asked again once the first was answered. They
        // share a panel instead (see app.rs), so decide never has a reason
        // to refuse an ask on those grounds.
        let c = Cone::new("park");
        let call = call_of("ask", r#"{"question":"Which?","options":["a","b"]}"#);

        let Dispatch::Park(ask) = decide(block(run(&c.root, &call, None))) else {
            panic!("a question should park")
        };
        assert_eq!(ask.question, "Which?");
        assert_eq!(ask.call_id, "call_1", "the answer has to find its way back");
    }

    #[test]
    fn several_servers_present_as_one_list_and_each_name_reaches_its_own() {
        let c = Cone::new("sources");
        let mut set = Sources::new();
        assert!(set.is_empty(), "a fresh set has no servers");
        set.push(Box::new(FakeMcp {
            prefix: "fs__",
            reply: Outcome::Done("files".into()),
        }));
        set.push(Box::new(FakeMcp {
            prefix: "db__",
            reply: Outcome::Done("database".into()),
        }));
        // Not merely cosmetic: the event loop passes `None` instead of this set
        // when it reports empty, so an `is_empty` stuck at true would make every
        // configured server unreachable while everything still built and ran.
        assert!(!set.is_empty(), "a set with servers in it is not empty");

        assert_eq!(
            block(run(&c.root, &call_of("fs__search", "{}"), Some(&set))),
            Outcome::Done("files".to_owned())
        );
        assert_eq!(
            block(run(&c.root, &call_of("db__search", "{}"), Some(&set))),
            Outcome::Done("database".to_owned()),
            "the second server is reachable, not shadowed by the first"
        );
        // One list, both servers in it -- checked through the trait as well as
        // directly, because the trait is what `run` and the request builder
        // actually reach for, and only `all_schemas` was being exercised.
        let direct: Vec<String> = set.all_schemas().into_iter().map(|(n, _, _)| n).collect();
        assert_eq!(
            direct,
            vec!["fs__search".to_owned(), "db__search".to_owned()]
        );

        let via_trait = McpTools::schemas(&set);
        assert_eq!(
            via_trait,
            set.all_schemas(),
            "the trait must not answer differently"
        );
        assert_eq!(
            via_trait
                .iter()
                .map(|(n, _, _)| n.as_str())
                .collect::<Vec<_>>(),
            vec!["fs__search", "db__search"]
        );
        // Descriptions and schemas travel too: a name with neither is a tool
        // the model is told about and cannot use.
        for (name, description, schema) in &via_trait {
            assert!(!description.is_empty(), "{name} has no description");
            assert!(!schema.is_empty(), "{name} has no schema");
        }

        // And the unknown-name message lists both servers, which is the path
        // that reads `schemas()` through the trait in anger.
        let Outcome::Refused(why) = block(run(&c.root, &call_of("nope", "{}"), Some(&set))) else {
            panic!("unknown names are refused")
        };
        assert!(
            why.contains("fs__search") && why.contains("db__search"),
            "{why}"
        );
    }

    #[test]
    fn a_source_that_changes_its_mind_is_refused_rather_than_panicking() {
        // `owns` and `call` are asked separately, so a source that claims a
        // name and then disowns it is possible in principle. It is still only
        // a name the model got wrong.
        struct Fickle;
        #[async_trait::async_trait]
        impl McpTools for Fickle {
            fn owns(&self, _: &str) -> bool {
                false
            }
            async fn call(&self, _: &Call) -> Outcome {
                Outcome::Done("unreachable".into())
            }
            fn schemas(&self) -> Vec<(String, String, String)> {
                Vec::new()
            }
        }
        let mut set = Sources::new();
        set.push(Box::new(Fickle));
        let out = block(McpTools::call(&set, &call_of("anything", "{}")));
        let Outcome::Refused(why) = out else {
            panic!("should refuse")
        };
        assert!(why.contains("no server offers"), "{why}");
    }

    #[test]
    fn an_empty_set_of_servers_behaves_as_no_servers_at_all() {
        // The ordinary case -- nobody has configured an MCP server -- must not
        // be a special path through the code.
        let c = Cone::new("no-sources");
        let set = Sources::new();
        assert!(set.is_empty());
        assert!(!set.owns("anything"));
        assert_eq!(
            block(run(
                &c.root,
                &call_of("file_read", r#"{"path":"inside.txt"}"#),
                Some(&set)
            )),
            Outcome::Done("in the cone".to_owned())
        );
    }

    #[test]
    fn a_remote_tool_is_routed_to_its_source() {
        let c = Cone::new("mcp-route");
        let mcp = FakeMcp {
            prefix: "fs__",
            reply: Outcome::Done("from the server".to_owned()),
        };
        let out = block(run(&c.root, &call_of("fs__search", "{}"), Some(&mcp)));
        assert_eq!(out, Outcome::Done("from the server".to_owned()));
    }

    #[test]
    fn a_server_cannot_take_over_a_local_tool_name() {
        // The whole reason names are namespaced at registration. A source that
        // claims to own everything still cannot be reached for `file_read`,
        // because local names are matched before ownership is ever consulted.
        let c = Cone::new("shadow");
        let greedy = FakeMcp {
            prefix: "",
            reply: Outcome::Done("hijacked".to_owned()),
        };
        let out = block(run(
            &c.root,
            &call_of("file_read", r#"{"path":"inside.txt"}"#),
            Some(&greedy),
        ));
        assert_eq!(
            out,
            Outcome::Done("in the cone".to_owned()),
            "a remote source answered for a local tool"
        );
        let out = block(run(
            &c.root,
            &call_of("ask", r#"{"question":"?"}"#),
            Some(&greedy),
        ));
        assert!(
            matches!(out, Outcome::Ask(_)),
            "a remote source answered for ask"
        );
    }

    #[test]
    fn an_unowned_remote_name_is_refused_with_the_remote_tools_listed_too() {
        // Listing only the local ones would tell a model that had just called
        // an MCP tool that no such thing exists anywhere.
        let c = Cone::new("mcp-unknown");
        let mcp = FakeMcp {
            prefix: "fs__",
            reply: Outcome::Done("unused".to_owned()),
        };
        let Outcome::Refused(why) = block(run(&c.root, &call_of("nope", "{}"), Some(&mcp))) else {
            panic!("unknown names are refused")
        };
        assert!(
            why.contains("fs__search"),
            "remote tools missing from the list: {why}"
        );
        assert!(
            why.contains("file_read"),
            "local tools missing from the list: {why}"
        );
    }

    #[test]
    fn every_local_tool_is_described_to_the_model_and_the_schema_is_valid_json() {
        // A tool the model is never told about is unreachable, and a schema it
        // cannot parse is worse than absent: the request fails rather than the
        // tool being skipped.
        let schemas = schemas();
        assert_eq!(
            schemas.len(),
            LOCAL.len(),
            "a tool exists with no schema, or the reverse"
        );
        for name in LOCAL {
            assert!(
                schemas.iter().any(|(n, _, _)| n == name),
                "{name} is dispatchable but never offered"
            );
        }
        for (name, description, schema) in &schemas {
            let parsed: Result<sonic_rs::Value, _> = sonic_rs::from_str(schema);
            assert!(
                parsed.is_ok(),
                "{name}'s schema is not JSON: {:?}",
                parsed.err()
            );
            assert!(
                description.len() > 40,
                "{name}'s description is what the model reads to decide whether \
                 the tool applies; a few words will not do it"
            );
        }
    }

    #[test]
    fn the_ask_schema_states_the_limit_the_code_enforces() {
        // Otherwise the model learns the bound by being refused, which costs a
        // turn to discover something we could have said up front.
        let (_, _, schema) = schemas()
            .into_iter()
            .find(|(n, _, _)| *n == "ask")
            .expect("ask has a schema");
        assert!(
            schema.contains(&format!("\"maxItems\": {MAX_OPTIONS}")),
            "the schema does not carry the option limit: {schema}"
        );
    }

    #[test]
    fn an_unknown_tool_is_refused_with_the_list_of_real_ones() {
        let call = Call {
            id: "call_1".into(),
            name: "rm_rf".into(),
            arguments: "{}".into(),
        };
        match block(run(Path::new("."), &call, None)) {
            Outcome::Refused(why) => {
                assert!(why.contains("rm_rf"), "{why}");
                for name in LOCAL {
                    assert!(why.contains(name), "should name {name}: {why}");
                }
            }
            other => panic!("unknown tools should be refused: {other:?}"),
        }
    }

    #[test]
    fn ask_carries_the_question_and_the_call_it_answers() {
        let out = ask(
            "call_7",
            r#"{"question":"Which one?","options":["a","b"],"multiple":true}"#,
        );
        assert_eq!(
            out,
            Outcome::Ask(Ask {
                call_id: "call_7".to_owned(),
                question: "Which one?".to_owned(),
                options: vec!["a".to_owned(), "b".to_owned()],
                multiple: true,
            })
        );
        // Options are optional: a bare question is answered by typing.
        let Outcome::Ask(a) = ask("call_8", r#"{"question":"Name?"}"#) else {
            panic!("should be an ask");
        };
        assert!(a.options.is_empty());
        assert!(!a.multiple, "single-select unless asked otherwise");
    }

    #[test]
    fn ask_refuses_more_options_than_the_panel_can_show() {
        let five = r#"{"question":"?","options":["a","b","c","d","e"]}"#;
        match ask("c", five) {
            Outcome::Refused(why) => assert!(why.contains("4"), "{why}"),
            other => panic!("five options should be refused: {other:?}"),
        }
        // Exactly the maximum is fine -- the boundary is inclusive.
        let four = r#"{"question":"?","options":["a","b","c","d"]}"#;
        assert!(matches!(ask("c", four), Outcome::Ask(_)));
    }

    #[test]
    fn ask_refuses_questions_and_options_nobody_could_read() {
        for bad in [
            r#"{"question":"   ","options":["a"]}"#,
            r#"{"question":"?","options":["a",""]}"#,
            r#"{"question":"?","options":["  ","b"]}"#,
            r#"{"options":["a"]}"#,
        ] {
            match ask("c", bad) {
                Outcome::Refused(_) => {}
                other => panic!("{bad:?} should be refused: {other:?}"),
            }
        }
    }
}
