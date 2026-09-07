# Terminal User Interface (TUI)

Kobold's TUI is designed for instant responsiveness, constant-time rendering, and minimal latency.

---

## Keyboard Shortcuts Cheat Sheet

| Key | Context | Action |
| :--- | :--- | :--- |
| **`Enter`** | Idle | Dispatch active prompt to the model. |
| **`Enter`** | Waiting (Busy) | **Queue prompt** in the lane's FIFO queue. |
| **`Shift + Enter`** / **`Alt + Enter`** / **`Ctrl + J`** | Prompt Bar | Insert a literal newline. |
| **`Esc`** | Queue not empty | **Unqueue**: Pop the most recently queued prompt back to the input bar. |
| **`Esc` `Esc`** (Double-Tap) | Waiting (Busy) | **Interrupt / Cancel**: Immediately halts the in-flight turn (`CancelTurn`). |
| **`Enter`** | Ask / Interrupt Panel | Confirm highlighted option or submit typed permission response. |
| **`Esc`** | Ask / Interrupt Panel | Reject or deny tool permission. |
| **`Ctrl + C`** | Global | Terminate TUI session. |
| **`Shift + Up` / `Down`** | Chat | Select messages in the transcript. |
| **`r`** | Message Selected | **Rewind**: Truncate conversation history to the selected checkpoint. |
| **`f`** | Message Selected | **Fork**: Branch execution into a new parallel lane from this checkpoint. |
| **`Shift + Left` / `Right`** | Global | Switch active focus between parallel lanes/panes. |
| **`Ctrl + W`** | Global | Close the currently focused lane pane. |
| **`Tab`** | Prompt Bar | Autocomplete slash commands (`/voice`, `/quit`, `/help`). |

---

## Prompt Queueing & Steering

### How to Queue
When the agent is executing tools or generating text (status is `Waiting`), type your next instruction and press **`Enter`**. Kobold automatically routes the message into the lane's FIFO queue and clears the input box. Once the active turn finishes, the queued prompt dispatches automatically.

### How to Steer Mid-Turn
If the agent is heading in the wrong direction:
1. Double-tap **`Esc`** (`Esc` `Esc`). The first press arms the interrupt latch; the second press cancels the in-flight turn.
2. The lane resets to `Ready`.
3. Type your course correction and press **`Enter`**.

---

## Hardware Progress Reporting (OSC 9;4)

Kobold emits standard terminal progress sequences (`OSC 9;4`) supported by modern terminal emulators (Ghostty, WezTerm, Windows Terminal, ConEmu). When a long turn runs, the terminal draws a smooth progress bar in the window title bar or tab header without requiring active focus.
