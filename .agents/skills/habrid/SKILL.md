---
name: habrid
version: 0.2.0
description: >-
  Hand a Beads discussion thread to another coding-agent harness via the habrid
  CLI, or behave as the invited target. Use when the user wants a second agent
  on a topic, when you would copy-paste a transcript into another CLI, or when
  your prompt says you were invoked by habrid. Inquiry: find what is true
  through disagreement; trust leads, verify claims.
---

# habrid

Beads is the transcript. Habrid is **one turn**. The caller decides whether to go again. Do not paste the whole discussion into the habrid command.

You may be the **caller** (you run `habrid`) or the **target** (habrid launched you). Both follow **Inquiry**.

## Inquiry (always)

Job: find what is **true and applicable**, not to agree with the caller or the last comment. Disagreement is the working medium. Socratic questions are in scope.

Other agents’ claims are **leads**. If they assert a flag, JSON field, measurement, or source: run it, open it, or mark it **unverified**. Do not treat another model’s prose as evidence.

A discuss-mode Beads comment (after the run token, if any) should include:

1. Position
2. Disagreements with the caller and prior turns (or “none, because …” plus a check)
3. Verified this turn (command, file, experiment)
4. Trusted unverified
5. Open questions
6. Ready to decide? `no` + what would settle it, or `yes` + proposed decision text

Assign-back is not a decision. When ready, record a decision bead (child or type `decision`): text, who, when, criteria, what was checked, remaining dissent. Close the thread only then.

Before creating a **new** epic, `bd search` for an existing topic. Reuse `--topic` / `--thread` across iterations.

## If you are the caller

`--from` is you. Positional arg is the target: `claude` | `grok` | `codex` | `agy` | `muse`.

Epic = `--topic`. Task (child of that epic) = `--thread`. Optional sub-tasks = goals. Multiple targets on the **same** thread are valid **in sequence**, not in parallel.

Put context on the thread (description, notes, comments) before launching. Extra argv is a short addendum only.

```bash
habrid <to> --from <you> --topic <epic-id> --thread <task-id> --model <model> --effort <effort>
```

Default is **sync**: wait in the foreground. That wait is the turn. After v0.2, exit 0 means a qualifying Beads reply for this run **and** assign-back (or accepted close).

If you cannot block:

```bash
habrid <to> --from <you> --topic … --thread … --async   # exit 0 = started
habrid wait <run-id>
# or poll: habrid status <run-id>
```

Never background a **sync** habrid and scrape logs. Never `--continue` on a target. Never `bd assign` the thread while habrid is live on it. Do not `--skip-beads` for a real handoff.

`--mode discuss` (default): read, web, `bd` — not a general shell. `--mode implement` when they should edit the tree. `--full-yolo` is invalid outside implement.

After return: `bd comments` / `bd show`. Expected success: assignee is you, `metadata.habrid_run` unset, target comment contains the run token. If the comment skipped disagreements **and** verification, it is not convergence — challenge or habrid again. Do not close the thread because two agents were polite.

## If you are the target

You were launched by habrid. Read the epic, thread, children (goals), and comments with `bd`. Follow the run token in your prompt: put it on the **first line** of your Beads comment. Prefer `bd comments add <thread> -a <you> -f <reply-path>`; if you cannot write that file, `bd comments add <thread> -a <you> -f -` on stdin. Do not require a Write tool. Then Inquiry sections above.

Do the work of **this** thread only. Do not start a parallel notes file. Do not invoke habrid unless a goal on this thread says to consult another harness.

When done: `bd assign <thread> <from>` (the caller). Leave the thread `in_progress` unless you recorded a decision and the goals are met. Close child goals only when they are actually met.

`discuss` mode: you may read the repo, search/fetch the web, and use `bd` as allowlisted. You may not general-purpose shell or rewrite the tree unless the prompt says `implement`.
