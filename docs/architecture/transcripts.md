# Append-Only Transcript DAG

Kobold does not store chat history as a flat array of messages. Instead, conversations are modeled as a **Directed Acyclic Graph (DAG)** of immutable turns recorded in an append-only JSONL log.

---

## File Format: `.kobold/transcript.jsonl`

Every turn appended to the transcript has a stable, deterministic cursor:

```json
{"branch":"main","seq":0,"parent":null,"who":"user","text":"Build an auth module."}
{"branch":"main","seq":1,"parent":null,"who":"model","text":"I will create auth.rs..."}
{"branch":"main-fork-1","seq":0,"parent":["main",1],"who":"user","text":"Actually, use JWT."}
```

### Transcript Cursor

```rust
pub struct Cursor<'a> {
    pub branch: &'a str,
    pub seq: usize,
    pub parent: Option<(&'a str, usize)>,
}
```

* **`branch`**: The name of the branch this turn belongs to (e.g. `main`, `main-fork-1`).
* **`seq`**: The zero-indexed position of this message within the branch.
* **`parent`**: If this branch was forked from an earlier branch, names the parent branch and the message index at which the fork split.

---

## Operations

### 1. Forking (`f` hotkey or `ClientFrame::Fork`)
* Creates an entirely new parallel lane without altering or deleting the parent history.
* Copies references up to the selected parent checkpoint.
* Both the original lane and the new fork can continue progressing independently.

### 2. Rewinding (`r` hotkey)
* Moves the active lane's branch head back to an earlier message index.
* Subsequent prompts branch forward from that cutoff, preserving the historical trail on disk.

### 3. Crash Recovery
* Rebuilding any branch requires reading the first `parent_at` messages of its parent (recursively), followed by its own messages in `seq` order.
* Because the transcript is strictly append-only, sudden power loss or process termination never corrupts existing turns.
