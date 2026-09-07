//! Interactive human-in-the-loop prompt detection for terminal agents.

/// A detected interactive prompt asking for human approval or input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedPrompt {
    pub question: String,
    pub options: Vec<String>,
}

/// Known patterns indicating an interactive terminal prompt waiting for human input.
pub fn detect_prompt(text: &str) -> Option<DetectedPrompt> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Take the last non-empty line (where prompts appear in terminal outputs)
    let last_line = trimmed.lines().rev().find(|l| !l.trim().is_empty())?.trim();
    let lower = last_line.to_lowercase();

    // Check for [y/n] / (y/n) style confirmation
    if lower.contains("[y/n]")
        || lower.contains("(y/n)")
        || lower.contains("[y/n]?")
        || lower.contains("(y/n)?")
        || lower.contains("[yes/no]")
        || lower.contains("(yes/no)")
    {
        return Some(DetectedPrompt {
            question: last_line.to_string(),
            options: vec!["Yes".into(), "No".into()],
        });
    }

    // Check for common phrasing
    if lower.contains("are you sure")
        || lower.contains("allow this command")
        || lower.contains("allow command")
        || lower.contains("proceed")
        || lower.contains("do you want to continue")
        || lower.contains("do you want to proceed")
    {
        return Some(DetectedPrompt {
            question: last_line.to_string(),
            options: vec!["Yes".into(), "No".into()],
        });
    }

    // Check for "Press Enter to continue"
    if lower.contains("press enter to continue") || lower.contains("press return to continue") {
        return Some(DetectedPrompt {
            question: last_line.to_string(),
            options: vec!["Continue".into()],
        });
    }

    None
}

/// Translates a human answer from `Command::ToolResult` back into terminal keystrokes.
pub fn translate_answer_to_keystrokes(answer: &str) -> String {
    let trimmed = answer.trim();
    if trimmed.eq_ignore_ascii_case("yes") || trimmed.eq_ignore_ascii_case("y") {
        "y\n".to_string()
    } else if trimmed.eq_ignore_ascii_case("no") || trimmed.eq_ignore_ascii_case("n") {
        "n\n".to_string()
    } else if trimmed.eq_ignore_ascii_case("continue") {
        "\n".to_string()
    } else {
        format!("{trimmed}\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detects_yes_no_brackets() {
        let p1 = detect_prompt("Do you want to proceed? [y/N]").expect("detect p1");
        assert_eq!(p1.question, "Do you want to proceed? [y/N]");
        assert_eq!(p1.options, vec!["Yes", "No"]);

        let p2 = detect_prompt("Apply changes (y/n)?").expect("detect p2");
        assert_eq!(p2.question, "Apply changes (y/n)?");
        assert_eq!(p2.options, vec!["Yes", "No"]);
    }

    #[test]
    fn test_detects_common_questions() {
        let p = detect_prompt("Are you sure you want to delete this?").expect("detect sure");
        assert_eq!(p.options, vec!["Yes", "No"]);

        let p2 = detect_prompt("Allow command?").expect("detect allow");
        assert_eq!(p2.options, vec!["Yes", "No"]);
    }

    #[test]
    fn test_detects_press_enter() {
        let p = detect_prompt("Press Enter to continue...").expect("detect enter");
        assert_eq!(p.options, vec!["Continue"]);
    }

    #[test]
    fn test_translates_answers() {
        assert_eq!(translate_answer_to_keystrokes("Yes"), "y\n");
        assert_eq!(translate_answer_to_keystrokes("y"), "y\n");
        assert_eq!(translate_answer_to_keystrokes("No"), "n\n");
        assert_eq!(translate_answer_to_keystrokes("n"), "n\n");
        assert_eq!(translate_answer_to_keystrokes("Continue"), "\n");
        assert_eq!(
            translate_answer_to_keystrokes("custom-input"),
            "custom-input\n"
        );
    }

    #[test]
    fn test_ignores_non_prompts() {
        assert!(detect_prompt("Building target v0.1.0...").is_none());
        assert!(detect_prompt("All 10 tests passed.").is_none());
    }
}
