# Headless Tmux & Persistent PTY Adapter

The `kobold-adapter-tmux` crate enables Kobold to manage long-running terminal sessions, interactive shells, and compiler loops inside persistent pseudo-terminals (PTY) or managed `tmux` sessions.

---

## Capabilities

* **Full ANSI Escape Parsing**: Strips or transforms raw terminal escapes into clean AG-UI text chunks.
* **Prompt & Interactive Input Interception**: Detects shell prompts (`$ `, `> `, `Password: `) and converts them into interactive questions.
* **Background Persistence**: If Kobold detaches or the frontend disconnects, the underlying shell or build continues running inside the persistent PTY/tmux session.
* **Ctrl-C Cancellation**: Propagates `Command::Cancel` as an immediate `SIGINT` (`\x03`) to the child process.

---

## Configuration & Usage

```sh
# Launch with standard PTY shell
./target/release/kobold --adapter ./target/release/kobold-adapter-tmux

# Launch with explicit command
./target/release/kobold \
  --adapter ./target/release/kobold-adapter-tmux \
  -- \
  --cmd "python3 -i"

# Wrap inside a persistent tmux session named "worker-1"
./target/release/kobold \
  --adapter ./target/release/kobold-adapter-tmux \
  -- \
  --tmux worker-1 \
  --cmd "bash"
```

---

## Interactive Prompts

When an interactive program requires user input (e.g. `Do you want to continue? [Y/n]`):
1. `kobold-adapter-tmux` intercepts the prompt using regex heuristics (`prompts.rs`).
2. Parks the lane and presents the question in the TUI or Web Companion.
3. Upon approval, writes the keystrokes directly to the master PTY descriptor.
