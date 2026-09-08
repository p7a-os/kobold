# Kobold Vision & Roadmap Alignment Audit

## 1. Request as understood
The user requested an evaluation of Kobold's purpose, architectural identity ("what is Kobold and why is it"), and roadmap ("what else should be"), specifically auditing current implementation distance and deviations from the ParadigmaOS vision document at `/Users/luis/w/kobold.pdf`.

## 2. Facts the decision depended on
- The vision document at `/Users/luis/w/kobold.pdf` establishes Kobold as a ParadigmaOS product acting as both a Harness (running tools, context, permissions, sandboxing) and a Meta-Harness (driving other harnesses such as Claude Code, Codex, and Grok Build over ACP).
- The codebase currently implements the dual-seam split, TUI, Web Companion, ACP adapter, PTY/tmux adapter, OpenAI WebSocket adapter, basic sandboxing (`bwrap`/`sandbox-exec`), and local egress proxy.
- Several strategic capabilities in `kobold.pdf` are absent or partially implemented: multi-model composition within a single task, automatic batch inference routing, credential injection at egress boundaries, mobile/desktop native surfaces, remote sandbox orchestration, and native integrations with Mithlond, Aleph, and Olympus.

## 3. Sources checked
1. Repository state and code:
   - `kobold-core/src/kernel.rs`, `kobold-core/src/lane.rs`, `kobold-core/src/daemon.rs`, `kobold-core/src/broker.rs`, `kobold-core/src/sandbox.rs`, `kobold-core/src/childenv.rs`.
   - `kobold-adapter-acp/src/main.rs`, `kobold-adapter-acp/src/bridge.rs`.
   - `src/app.rs`, `src/wizard.rs`, `src/catalog.rs`.
2. Vision document:
   - `/Users/luis/w/kobold.pdf` (Pages 1 to 4).
3. Project documentation:
   - `docs/architecture/seams.md`, `docs/agui.md`, `docs/configuration/worktrees.md`, `docs/configuration/mcp.md`.

## 4. Observations
- Page 1 ("The Shape of It"): The dual role of Harness (runs loop) vs Meta-Harness (drives others) is fundamentally sound and matches our `OutgoingFrame`/`IncomingFrame` and ACP architecture.
- Page 2 ("The Unused Asset"): The vision describes combining models per task (long-context reads, reasoning decides, fast drafts; consensus adjudication) and automated batch routing for non-interactive work. Today, a lane executes strictly against one model/adapter at a time. No batch queue or multi-model task composer exists in `kobold-core`.
- Page 2 ("Surfaces"): Terminal (TUI) and Web Companion exist. Desktop (native window) and Mobile are absent. Remote sandboxes surviving laptop lid close are supported locally via `koboldd` daemon detach, but remote cloud sandbox hosting is absent.
- Page 3 ("Confinement"): Bubblewrap and Seatbelt confinement exist, and environment variables are stripped. However, credential injection at the egress boundary (TLS end-to-end secret splice where neither Kobold nor adapter holds the raw key) is not yet implemented; Kobold passes API keys via `Startup` stdin.
- Page 3 ("ParadigmaOS"): Seamless integration with Mithlond (gateway), Aleph (evidence memory), and Olympus (workflow). `.knowledge/` provides the local prototype of Aleph's memory schema, but live network APIs to Mithlond and Olympus are pending.
- Page 4 ("Where Kobold Differs"): Core competitive differentiators are verified: speaking native protocols, driving other harnesses via ACP, client-owned DAG transcripts, and local sandboxing.

## 5. Decision
Recorded vision concepts into ubiquitous language and project knowledge base:
- Added canonical terms: `T-meta-harness`, `T-batch-inference`, `T-aleph`, `T-mithlond`, `T-olympus` to `.knowledge/dictionary.md`.
- Added fact `F-15` documenting vision requirements to `.knowledge/facts.md`.
- Added intent `I-4` to `.knowledge/intents.md`.

## 6. Verification
Follow these steps to run the verification instrument:
1. Open a terminal in the repository root directory.
2. Execute the verification script:
   ```sh
   ./.agent-reports/2026-09-08-knowledge-base-init/verify.sh
   ```
3. Confirm that the script exits with code 0 and reports all schema checks passed.

Verification Result:
The script verified that all four knowledge base files exist, contain `## Current` and `## History` sections, and strictly follow the required ID naming schemas and field structures. All 56 entries passed validation without error.

## 7. Unverified and risks
- Live Mithlond and Olympus API contracts are unverified; they are external services in ParadigmaOS.
- Risk: Multi-model composition within a single task will require expanding `kobold-core::lane::LaneEffect` to support sub-task delegation and multi-agent synthesis.

## 8. Out-of-scope findings
- None.
