# Project Review & Initial Knowledge Base

## 1. Request as understood
The user requested a thorough review of the Kobold repository and the initialization of the project knowledge base under `.knowledge/`. The user also requested an interview through the question tool to resolve open product questions.

## 2. Facts the decision depended on
- The project lacked a `.knowledge/` directory (`dictionary.md`, `facts.md`, `intents.md`, `decisions.md`).
- Rule §5 requires creating and committing the initial knowledge base on the first session when `.knowledge/` does not exist.
- Rule §2 requires committing changes to a feature branch rather than the default branch.
- Rule §3 requires routing all human decisions, clarifications, and product choices through the question tool.

## 3. Sources checked
1. Codebase repository and filesystem:
   - Root configuration: `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`.
   - Documentation tree: `README.md`, `docs/README.md`, `docs/architecture/`, `docs/adapters/`, `docs/configuration/`, `docs/usage/`, `docs/performance/`.
   - Core crates: `kobold-core`, `kobold-proto`, `kobold-adapter-acp`, `kobold-adapter-tmux`, `kobold-openai`, `kobold-tts`, and root binary crate `kobold`.
   - Tests: Integration and contract test suites under `tests/`.
2. Project documentation:
   - `docs/agui.md`: Protocol subset specifications and state machine boundary findings.
   - `docs/architecture/seams.md`: Dual-seam architectural contracts.
   - `docs/configuration/worktrees.md`: Concurrency leasing and git worktree isolation.
3. Official protocols:
   - Agent Client Protocol (ACP) specification.
   - AG-UI Protocol specification.

## 4. Observations
- `cargo check --workspace --all-targets` completed cleanly with exit code 0.
- `kobold-proto` defines protocol types for both the Northbound seam (`ClientFrame`, `ServerFrame`, `ClientServerFrame`) and the Southbound seam (`OutgoingFrame`, `IncomingFrame`, `Command`, `Startup`).
- `kobold-core::Kernel` manages execution lanes, prompt queuing, tool runs, and transcript DAG appending.
- `docs/agui.md:576-604` documents an unresolved gap where `park_ask` leaves lane status unmodified, allowing queued prompts to execute while an interactive question is open.
- `src/wizard.rs:362-363` marks direct Anthropic and Google adapters as disabled in the setup wizard.
- `kobold-core/src/sandbox.rs:408-417` notes an unverified assumption regarding macOS Seatbelt handling of Unix domain sockets under network deny policies.

## 5. Decision
Initialized `.knowledge/` with canonical terminology, established facts, active intents, and architectural decisions:
- `.knowledge/dictionary.md`: 24 canonical terms (`T-kobold` through `T-doctor`, `T-encrypted-reasoning`, and `T-habrid`).
- `.knowledge/facts.md`: 14 verified/assumed facts (`F-1` through `F-14`).
- `.knowledge/intents.md`: 3 intents (`I-1` through `I-3`, with `I-3` marked `done` upon completing the interview).
- `.knowledge/decisions.md`: 9 architectural decisions (`D-1` through `D-5` from architecture docs, and `D-6` through `D-9` from the user interview):
  - `D-6`: Freeze prompt queue during parked tool questions until full interaction completes.
  - `D-7`: Implement AG-UI encrypted reasoning pass-through (`REASONING_ENCRYPTED_VALUE`) under `store: false`.
  - `D-8`: Prioritize the ACP adapter as the primary multi-agent interface; keep direct streaming focused on OpenAI/OpenRouter.
  - `D-9`: Retain conversation forks in current workspace by default; create git worktree on demand.

## 6. Verification
Follow these steps to run the verification instrument:
1. Open a terminal in the repository root directory.
2. Execute the verification script:
   ```sh
   ./.agent-reports/2026-09-08-knowledge-base-init/verify.sh
   ```
3. Confirm that the script exits with code 0 and reports all schema checks passed.

Verification Result:
The script verified that all four knowledge base files exist, contain `## Current` and `## History` sections, and strictly follow the required ID naming schemas and field structures. All 50 entries passed validation without error.

## 7. Unverified and risks
- Fact `F-13`: macOS Seatbelt network-outbound exception for Unix domain sockets remains unverified on live macOS runtime without an active sandbox-exec test pass.
- Risk: Implementation of `D-6` (freezing prompt queue across tool questions) will require an explicit parked state in `kobold-core::lane::Lane` to prevent queue draining on `Completed` events.

## 8. Out-of-scope findings
- `docs/agui.md:576-604` reports that `park_ask` does not touch `pane.status`, allowing queued messages to drain during open questions. Resolved strategically via decision `D-6`; implementation will follow in a subsequent task.
