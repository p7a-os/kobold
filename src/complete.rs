//! Completion for the slash commands.
//!
//! Slash commands are local UI controls rather than anything the model sees,
//! so the set is small, fixed and known at compile time. That makes matching a
//! prefix scan and lets the suggestion list, the `Tab` completion and `/help`
//! all read from one table instead of drifting apart.

/// A local command and the one-line description shown beside it.
pub struct Command {
    pub name: &'static str,
    pub help: &'static str,
}

pub const COMMANDS: &[Command] = &[
    Command {
        name: "model",
        help: "switch model or list available ([model][@provider])",
    },
    Command {
        name: "backend",
        help: "switch active agent/harness or list available",
    },
    Command {
        name: "harness",
        help: "alias for /backend",
    },
    Command {
        name: "voice",
        help: "speak replies aloud",
    },
    Command {
        name: "detach",
        help: "disconnect and leave daemon running",
    },
    Command {
        name: "quit",
        help: "exit kobold",
    },
    Command {
        name: "help",
        help: "list these commands",
    },
];

/// What has been typed after the leading slash, when the input is still naming
/// a command.
///
/// `None` once any whitespace has been typed: at that point the name is
/// settled and the user is writing an argument, so continuing to offer
/// completions would be arguing with them.
pub fn typed(input: &str) -> Option<&str> {
    let rest = input.strip_prefix('/')?;
    if rest.contains(char::is_whitespace) {
        return None;
    }
    Some(rest)
}

/// Commands whose name begins with what has been typed. Empty when the input
/// is not naming a command at all, which is also what closes the suggestion
/// list.
pub fn matches(input: &str) -> Vec<&'static Command> {
    let Some(prefix) = typed(input) else {
        return Vec::new();
    };
    COMMANDS
        .iter()
        .filter(|c| c.name.starts_with(prefix))
        .collect()
}

/// The direction a toggle would move, when a command has one.
///
/// Only ever the direction that would change something: offering `off` while
/// speech is already off reads as a description of the current state rather
/// than as something to type.
fn toggle(name: &str, voice_on: bool) -> Option<&'static str> {
    match name {
        "voice" => Some(if voice_on { "off" } else { "on" }),
        _ => None,
    }
}

/// Everything a command accepts, in the order `Tab` offers them.
///
/// The toggle first, since it is the argument most often wanted, then the
/// voices in the order they are declared.
pub fn args(name: &str, voice_on: bool) -> Vec<String> {
    if COMMANDS.iter().all(|c| c.name != name) {
        return Vec::new();
    }
    let mut out: Vec<String> = toggle(name, voice_on)
        .map(str::to_owned)
        .into_iter()
        .collect();
    if name == "voice" {
        out.extend(crate::tts::VOICES.iter().map(|v| (*v).to_owned()));
    }
    if name == "backend" || name == "harness" {
        out.extend(
            [
                "claude-code",
                "grok-build",
                "codex",
                "opencode",
                "antigravity",
                "openai",
                "openrouter",
            ]
            .iter()
            .map(|&s| s.to_owned()),
        );
    }
    if name == "model" {
        out.extend(
            [
                "sonnet-5",
                "opus-5",
                "haiku-4.5",
                "grok-4.6",
                "grok-4.5",
                "gpt-5.6-luna",
                "gpt-5.6-terra",
                "gpt-6-astra",
            ]
            .iter()
            .map(|&s| s.to_owned()),
        );
    }
    out
}

/// A summary of the above for the dim hint that trails the input: the toggle
/// spelled out, and the voices as a placeholder because naming all eight would
/// be longer than the prompt.
///
/// `None` once an argument has been started, since by then the hint has been
/// read and is only in the way, and `None` for commands taking nothing so
/// `/quit` does not sprout an empty one.
pub fn hint(input: &str, voice_on: bool) -> Option<String> {
    let rest = input.strip_prefix('/')?;
    let (name, tail) = rest.split_once(' ').unwrap_or((rest, ""));
    if !tail.trim().is_empty() {
        return None;
    }
    if args(name, voice_on).is_empty() {
        return None;
    }
    let mut parts: Vec<String> = toggle(name, voice_on)
        .map(str::to_owned)
        .into_iter()
        .collect();
    if name == "voice" {
        parts.push("<name>".to_owned());
    }
    if name == "model" {
        parts.push("<model>[@provider]".to_owned());
    }
    if name == "backend" || name == "harness" {
        parts.push("<harness>".to_owned());
    }
    Some(parts.join("   "))
}

/// The line `Tab` should produce when the command is already named: the next
/// argument along.
///
/// Cycles. An argument that already matches one in the list steps to the one
/// after it and wraps at the end, so repeated presses walk the whole set. One
/// partly typed jumps to the first argument continuing it, which makes
/// `/voice al` then Tab land on `alba` rather than starting over.
///
/// `None` while the name itself is still being typed -- Tab is completing the
/// name then -- and `None` for commands that take nothing.
pub fn next_arg(input: &str, voice_on: bool) -> Option<String> {
    let rest = input.strip_prefix('/')?;
    // The space is what says the name is settled.
    let (name, tail) = rest.split_once(' ')?;
    let list = args(name, voice_on);
    if list.is_empty() {
        return None;
    }
    let current = tail.trim();
    let next = match list.iter().position(|a| a == current) {
        Some(i) => (i + 1) % list.len(),
        None => list
            .iter()
            .position(|a| a.starts_with(current))
            .unwrap_or(0),
    };
    Some(format!("/{name} {}", list[next]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(input: &str) -> Vec<&str> {
        matches(input).iter().map(|c| c.name).collect()
    }

    #[test]
    fn a_bare_slash_offers_everything_and_a_prefix_narrows_it() {
        assert_eq!(
            names("/"),
            vec!["model", "backend", "harness", "voice", "detach", "quit", "help"]
        );
        assert_eq!(names("/m"), vec!["model"]);
        assert_eq!(names("/b"), vec!["backend"]);
        assert_eq!(names("/v"), vec!["voice"]);
        assert_eq!(names("/d"), vec!["detach"]);
        assert_eq!(names("/h"), vec!["harness", "help"]);
        // The `/q` alias is a prefix of `quit`, so it needs no special case.
        assert_eq!(names("/q"), vec!["quit"]);
        assert!(names("/zzz").is_empty());
    }

    #[test]
    fn ordinary_text_is_never_a_completion() {
        // The suggestion list is driven entirely by this, so anything that is
        // not naming a command has to come back empty.
        for input in ["", "hello", "what is /voice", "  /voice"] {
            assert!(names(input).is_empty(), "{input:?} should not complete");
        }
    }

    #[test]
    fn a_started_argument_closes_the_list_but_a_full_name_does_not() {
        // Still naming the command: the list stays up on an exact match, so
        // Tab can add the space.
        assert_eq!(names("/voice"), vec!["voice"]);
        // Past the space the name is settled.
        assert!(names("/voice ").is_empty());
        assert!(names("/voice al").is_empty());
    }

    #[test]
    fn the_voice_hint_offers_only_the_useful_direction() {
        assert_eq!(hint("/voice", false).as_deref(), Some("on   <name>"));
        assert_eq!(hint("/voice", true).as_deref(), Some("off   <name>"));
        // A trailing space is a completed command, not an argument.
        assert_eq!(hint("/voice ", false).as_deref(), Some("on   <name>"));
        // Once an argument is being typed the hint has done its job.
        assert_eq!(hint("/voice al", false), None);
    }

    #[test]
    fn tab_walks_the_arguments_and_wraps() {
        // The toggle first, then every voice, then back to the start.
        let mut line = "/voice ".to_owned();
        let mut seen = Vec::new();
        for _ in 0..crate::tts::VOICES.len() + 1 {
            line = next_arg(&line, false).expect("an argument");
            seen.push(line.clone());
        }
        assert_eq!(seen[0], "/voice on", "the toggle comes first");
        assert_eq!(seen[1], "/voice alba");
        assert_eq!(seen[2], "/voice marius");
        assert_eq!(seen.last().unwrap(), "/voice azelma");
        // One more wraps to the beginning rather than stopping.
        assert_eq!(
            next_arg(seen.last().unwrap(), false).as_deref(),
            Some("/voice on")
        );
    }

    #[test]
    fn the_toggle_offered_follows_the_current_state() {
        assert_eq!(next_arg("/voice ", false).as_deref(), Some("/voice on"));
        assert_eq!(next_arg("/voice ", true).as_deref(), Some("/voice off"));
        // And stepping off it reaches the voices either way.
        assert_eq!(next_arg("/voice off", true).as_deref(), Some("/voice alba"));
        assert_eq!(next_arg("/voice on", false).as_deref(), Some("/voice alba"));
    }

    #[test]
    fn a_partly_typed_argument_jumps_to_what_continues_it() {
        assert_eq!(next_arg("/voice al", false).as_deref(), Some("/voice alba"));
        assert_eq!(
            next_arg("/voice j", false).as_deref(),
            Some("/voice javert")
        );
        // Then it is an exact match, so the next press steps along.
        assert_eq!(
            next_arg("/voice javert", false).as_deref(),
            Some("/voice jean")
        );
        // Nothing continues it, so start from the top rather than refusing.
        assert_eq!(next_arg("/voice zzz", false).as_deref(), Some("/voice on"));
    }

    #[test]
    fn arguments_are_only_offered_once_the_name_is_settled() {
        // Still naming: Tab belongs to the command list here.
        assert_eq!(next_arg("/voi", false), None);
        assert_eq!(next_arg("/voice", false), None);
        // Nothing to cycle for a command that takes nothing.
        assert_eq!(next_arg("/quit ", false), None);
        assert_eq!(next_arg("/help ", false), None);
        // Not a command at all.
        assert_eq!(next_arg("/nonsense ", false), None);
        assert_eq!(next_arg("hello there", false), None);
    }

    #[test]
    fn every_voice_is_reachable_by_tabbing() {
        // The point of the rotation: no voice is stranded where only typing
        // its name would find it.
        let mut line = "/voice ".to_owned();
        let mut reached = std::collections::HashSet::new();
        for _ in 0..64 {
            line = next_arg(&line, false).expect("an argument");
            reached.insert(line.trim_start_matches("/voice ").to_owned());
        }
        for v in crate::tts::VOICES {
            assert!(reached.contains(*v), "{v} cannot be reached by tabbing");
        }
    }

    #[test]
    fn commands_without_arguments_have_no_hint() {
        assert_eq!(hint("/quit", false), None);
        assert_eq!(hint("/help", false), None);
        // Not a command at all.
        assert_eq!(hint("/nonsense", false), None);
        assert_eq!(hint("hello", false), None);
    }
}
