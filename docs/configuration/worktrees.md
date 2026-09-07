# Worktree Concurrency & UI Leases

Kobold provides native safety primitives for multi-client collaboration and multi-instance workspace isolation.

---

## Single-Writer UI Lease

Only **one** attached UI client holds write authority on a `koboldd` daemon at any time.

* **Primary Client**: Receives full read-write lease (`rw_client_id`). Can submit prompts, cancel turns, and submit interrupts.
* **Secondary / Follower Clients**: Automatically downgraded to **Read-Only Follower Mode**.
  * Can observe the live stream, status changes, and history.
  * Any attempt to submit a prompt or answer is rejected with a notice frame (`Command ignored: client is attached in read-only mode`).
* **Dynamic Promotion**: If the primary read-write client disconnects, the next attached follower is promoted to read-write mode automatically.

---

## Collision Detection & Worktree Generation

When starting a new Kobold daemon in a directory where another instance is already running:

1. **Conflict Detection**: Kobold identifies that an active daemon socket is already listening on the workspace.
2. **Git Repository Verification**: Checks whether the current working directory is a git repository.
3. **Automated Worktree Creation**: Generates an isolated git worktree named using the format:
   $$\{number\}-\{adjective\}-\{vehicle\}-\{2\text{ random hex bytes}\}$$
   * **Number**: 1 to 20 spelled out (e.g. `eleven`, `one`).
   * **Adjectives**: 256 colors, moods, sizes, textures (e.g. `pink`, `silent`, `swift`).
   * **Vehicles**: 256 transport machines (e.g. `trains`, `car`, `glider`, `rover`).
   * *Example*: `eleven-pink-trains-a1b2`, `one-big-car-4f9e`.
4. **Execution in Worktree**: Starts the new Kobold instance inside the freshly created worktree, preventing git state conflicts and file collisions.
