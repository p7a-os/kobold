//! Append-only session log at `.kobold/transcript.jsonl`.
//!
//! One JSON object per line so a partially written session is still readable
//! and a crash can only ever lose the last line. Doubles as the source for
//! input history across restarts.

use std::borrow::Cow;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::lane::Who;

pub const DIR: &str = ".kobold";
pub const FILE: &str = "transcript.jsonl";

/// A conversation is a tree, not a line: forking opens a branch, and a rewind
/// is the same cut made in place. Each record therefore names its branch and,
/// for a branch's records, where it was cut from its parent.
///
/// Reconstructing a branch: take the first `parent_at` messages of the parent
/// branch (recursively), then this branch's own records in `seq` order.
#[derive(Serialize, Deserialize)]
pub struct Record<'a> {
    /// Unix seconds. Absolute, so a log read months later still means something.
    pub ts: u64,
    #[serde(borrow)]
    pub branch: Cow<'a, str>,
    /// Index of this record within its own branch.
    pub seq: usize,
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub parent: Option<Cow<'a, str>>,
    /// How many of the parent's messages this branch inherited before diverging.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_at: Option<usize>,
    #[serde(borrow)]
    pub role: Cow<'a, str>,
    /// `Cow`, not `&str`: any message containing a quote or newline is escaped
    /// in the JSON and cannot be borrowed out of the line buffer. With `&str`
    /// those records fail to parse and silently vanish from history.
    #[serde(borrow)]
    pub text: Cow<'a, str>,
}

/// Where the next record of a branch goes.
pub struct Cursor<'a> {
    pub branch: &'a str,
    pub seq: usize,
    pub parent: Option<(&'a str, usize)>,
}

pub struct Log {
    /// `None` when the log could not be opened. A read-only checkout must not
    /// stop the session, so logging degrades to silence rather than failing.
    file: Option<File>,
}

impl Log {
    pub fn open(root: &Path) -> Self {
        let dir = root.join(DIR);
        let file = std::fs::create_dir_all(&dir)
            .and_then(|_| {
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(dir.join(FILE))
            })
            .ok();
        Self { file }
    }

    pub fn append(&mut self, at: Cursor<'_>, who: Who, text: &str) {
        let Some(f) = self.file.as_mut() else { return };
        let role = match who {
            Who::User => "user",
            Who::Model => "model",
            Who::System => "system",
        };
        let rec = Record {
            ts: now(),
            branch: Cow::Borrowed(at.branch),
            seq: at.seq,
            parent: at.parent.map(|(b, _)| Cow::Borrowed(b)),
            parent_at: at.parent.map(|(_, n)| n),
            role: Cow::Borrowed(role),
            text: Cow::Borrowed(text),
        };
        if let Ok(line) = crate::json::to_string(&rec) {
            let _ = writeln!(f, "{line}");
            // Flushed per record: an agent session can be killed at any moment
            // and a buffered tail would be lost.
            let _ = f.flush();
        }
    }
}

pub fn path(root: &Path) -> PathBuf {
    root.join(DIR).join(FILE)
}

/// Past user messages, oldest first, for input history. Malformed lines are
/// skipped rather than aborting: a truncated last line is normal after a kill.
pub fn user_history(root: &Path) -> Vec<String> {
    let Ok(f) = File::open(path(root)) else {
        return Vec::new();
    };
    BufReader::new(f)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| {
            let rec: Record = crate::json::from_slice(line.as_bytes()).ok()?;
            (rec.role == "user").then(|| rec.text.into_owned())
        })
        .collect()
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real directory, removed on drop. The log degrades to silence on an
    /// unopenable path by design, so a test that used one would hand this
    /// module a fixture that swallows everything it does -- the trap that
    /// left five mutants alive in `absorb`.
    struct Dir(PathBuf);

    impl Dir {
        fn new(name: &str) -> Self {
            let p = std::env::temp_dir()
                .join(format!("kobold-transcript-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).expect("temp dir");
            Self(p)
        }
        fn write(&self, at: Cursor<'_>, who: Who, text: &str) {
            Log::open(&self.0).append(at, who, text);
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn at(seq: usize) -> Cursor<'static> {
        Cursor {
            branch: "b0",
            seq,
            parent: None,
        }
    }

    #[test]
    fn the_log_lives_under_the_directory_it_was_given() {
        // Asserted against the root rather than against a constant: a `path`
        // that ignored its argument would still satisfy a test that only
        // checked the tail, and every caller passes a different root.
        let root = Path::new("/some/project");
        assert_eq!(
            path(root),
            Path::new("/some/project/.kobold/transcript.jsonl")
        );
        assert!(
            path(Path::new("/other")).starts_with("/other"),
            "the root must be honoured"
        );
    }

    #[test]
    fn history_is_the_users_own_messages_oldest_first() {
        let d = Dir::new("history");
        d.write(at(0), Who::User, "first");
        d.write(at(1), Who::Model, "a reply");
        d.write(at(2), Who::User, "second");

        // Order and content both, because a reversed or truncated history is
        // as wrong as an empty one and an emptiness check cannot see it.
        assert_eq!(
            user_history(&d.0),
            vec!["first".to_owned(), "second".to_owned()]
        );
    }

    #[test]
    fn only_the_user_is_history_and_the_role_test_is_not_inverted() {
        // The partner to the test above. `role == "user"` inverted returns
        // the model's words as the user's own, and up-arrow would then offer
        // the model's text as something to resend.
        let d = Dir::new("roles");
        d.write(at(0), Who::Model, "the model spoke");
        d.write(at(1), Who::System, "a notice");

        let got = user_history(&d.0);
        assert!(got.is_empty(), "a non-user role reached history: {got:?}");
    }

    #[test]
    fn a_truncated_last_line_is_skipped_rather_than_losing_the_history_before_it() {
        // Normal after a kill: the process died mid-write. The doc promises
        // the rest survives, and a parser that aborted on the first bad line
        // would silently empty a user's history.
        let d = Dir::new("truncated");
        d.write(at(0), Who::User, "kept");
        let mut f = OpenOptions::new()
            .append(true)
            .open(path(&d.0))
            .expect("append");
        write!(f, "{{\"ts\":1,\"branch\":\"b0\",\"seq\":1,\"rol").expect("partial line");
        drop(f);

        assert_eq!(user_history(&d.0), vec!["kept".to_owned()]);
    }

    #[test]
    fn text_containing_quotes_and_newlines_survives_the_round_trip() {
        // The reason `text` is a `Cow` rather than a `&str`: an escaped
        // string cannot be borrowed out of the line buffer, and with `&str`
        // those records fail to parse and vanish from history. That is the
        // failure this asserts against, and it is invisible to plain text.
        let d = Dir::new("escapes");
        let nasty = "he said \"no\"\nthen left\ttabbed";
        d.write(at(0), Who::User, nasty);
        assert_eq!(user_history(&d.0), vec![nasty.to_owned()]);
    }

    #[test]
    fn a_missing_log_is_an_empty_history_rather_than_a_failure() {
        let d = Dir::new("absent");
        assert!(user_history(&d.0).is_empty());
        assert!(user_history(Path::new("/nonexistent/path")).is_empty());
    }

    #[test]
    fn a_record_is_stamped_with_a_real_wall_clock_second() {
        // `ts` is documented as absolute so a log read months later still
        // means something. A stamp of 0 or 1 is 1970, which is not a time
        // any record was written and would make every record look
        // simultaneous.
        let d = Dir::new("stamp");
        d.write(at(0), Who::User, "when");

        let line = std::fs::read_to_string(path(&d.0)).expect("read back");
        let rec: Record = crate::json::from_slice(line.as_bytes()).expect("one record");
        // 1_700_000_000 is November 2023. Any clock behind that is wrong in a
        // way worth failing on rather than a plausible-looking small number.
        assert!(
            rec.ts > 1_700_000_000,
            "ts was {} -- not a wall-clock second",
            rec.ts
        );
    }
}
