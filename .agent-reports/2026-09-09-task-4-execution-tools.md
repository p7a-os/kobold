# TASK-4: Execution Tools (kobold-tool-fs & kobold-tool-bash)

## 1. Request as understood

Build `crates/kobold-tool-fs` and `crates/kobold-tool-bash` implementing the execution tools for the Kobold thin harness. Filesystem tools (`read_file`, `write_file`, `edit_file`, `list_dir`) must enforce strict cone confinement to prevent path traversal or symlink escapes, attach pruning policies (`KeepLast`, `CollapseAfterTurns`), and declare `is_mutating` accurately. The bash tool (`bash`) must support one-shot and persistent sessions, dual sandboxing (`HostProcess` and `MicroVm`), APFS copy-on-write snapshots (`clonefile`) with checkpoint rollback, timeouts, and `HeadTail` pruning.

## 2. Facts the decision depended on

- `kobold-tool-fs` and `kobold-tool-bash` implement the `Tool` trait from `kobold-types` (`D-10`, `D-11`).
- Read-only tools (`read_file`, `list_dir`) return `is_mutating = false` to enable 0ms, 0MB host execution while the workspace is clean (`D-30`, `D-39`).
- Mutating tools (`write_file`, `edit_file`, `bash`) return `is_mutating = true` (`D-30`, `D-39`).
- In `MicroVm` mode, both filesystem and shell execution operate on the same filesystem target to prevent split-brain states (`D-35`, `D-36`).
- Dual sandbox mode supports `HostProcess` via opt-in `--sandbox host` and `MicroVm` via vsock guest driver (`D-36`).
- Checkpoints on macOS utilize APFS CoW (`clonefile`) for instant snapshot creation and rollback (`D-35`).
- Non-zero process exit codes feed back into conversation context as tool errors (`D-16`).

## 3. Sources checked

1. `.tasks.md:51-73` - Verified TASK-4 scope and acceptance criteria.
2. `.knowledge/decisions.md:D-16, D-30, D-35..D-39` - Checked architectural decisions for sandboxing, tools, and mutation reporting.
3. `kobold-core/src/tools.rs:65-179` - Inspected canonical cone security resolution algorithms.
4. macOS `Virtualization.framework` and `libc::clonefile` manual pages.

## 4. Observations

- `resolve_in_cone` prevents directory traversal by checking normalized ancestor paths and comparing real destinations against the canonical workspace root.
- Read operations can specify optional 1-indexed line ranges (`start_line`, `end_line`) for fine-grained reading.
- Targeted file editing verifies single-occurrence matches unless `allow_multiple` is explicitly enabled, preventing accidental multi-site corruption.
- The `bash` tool captures standard output, standard error, and exit codes. Non-zero exit codes return `ToolOutput::error` with details for model self-correction.
- Persistent sessions support consecutive commands retaining shell state, variables, and working directories.
- APFS `clonefile` creates directory snapshots without copying data blocks on disk, enabling rollback within milliseconds.

## 5. Decision

Implemented `crates/kobold-tool-fs`:
- `cone.rs`: `resolve_in_cone`, `normalise_path`, and `outside_cone_error` enforcing strict sandbox bounds.
- `read.rs`: `ReadFileTool` with line range slicing and `KeepLast` pruning policy (`is_mutating: false`).
- `write.rs`: `WriteFileTool` with parent directory creation and overwrite guards (`is_mutating: true`).
- `edit.rs`: `EditFileTool` with precise string search and replacement (`is_mutating: true`).
- `list.rs`: `ListDirTool` with iterative breadth-first search directory traversal (`is_mutating: false`).
- `lib.rs`: `create_fs_tools` factory and unit test suite.

Implemented `crates/kobold-tool-bash`:
- `checkpoint.rs`: `CheckpointManager` implementing APFS CoW snapshot creation and rewind via `libc::clonefile`.
- `sandbox.rs`: `SandboxExecutor`, `HostProcessExecutor` with one-shot and persistent sessions, and `MicroVmExecutor` vsock driver abstraction.
- `tool.rs`: `BashTool` coordinating execution, timeout management, output truncation, and `HeadTail` pruning policy (`is_mutating: true`).
- `lib.rs`: `create_host_bash_tool` factory and unit test suite.

## 6. Verification

Run the verification instrument:

1. Open a terminal in the repository root directory.
2. Execute the verification script:
   ```bash
   .agent-reports/2026-09-09-task-4-execution-tools/verify.sh
   ```
3. Confirm that all 5 tests pass for `kobold-tool-fs`.
4. Confirm that all 6 tests pass for `kobold-tool-bash`.
5. Confirm that all 36 tests pass across the entire crate workspace.
6. Confirm that `cargo clippy` emits zero warnings across all crates.

Observed output:
- `cargo check -p kobold-tool-fs -p kobold-tool-bash`: exit code 0.
- `cargo test -p kobold-tool-fs -p kobold-tool-bash`: 11 passed, 0 failed.
- Full workspace test suite: 36 passed, 0 failed.
- `cargo clippy -p kobold-tool-fs -p kobold-tool-bash`: exit code 0.

## 7. Unverified and risks

- Running microVM vsock communication requires code signing with the `com.apple.security.virtualization` entitlement when deploying the standalone daemon binary on macOS. The `MicroVmExecutor` driver abstraction handles wire framing and will connect to live hypervisor instances during daemon integration (TASK-7).

## 8. Out-of-scope findings

- OpenAI streaming client (`kobold-backend-openai`) belongs to TASK-5.
- SDK session runtime binding (`kobold-runtime`) belongs to TASK-6.
