# Web Companion UI

Kobold includes a built-in, lightweight web companion server that serves a real-time web interface on `http://127.0.0.1:3000`.

---

## Starting the Web Companion

The web companion is automatically spawned when `koboldd` is launched with `--ws` or when running through the launcher:

```sh
# Start daemon with Web companion on port 3000
./target/release/koboldd --ws-port 3000 --session my-session
```

Upon binding, Kobold generates a cryptographically secure 256-bit authentication token and writes it to:
```sh
/tmp/kobold-{uid}/{session_id}.auth
```

A direct access URL is printed to the console:
```sh
http://127.0.0.1:3000/?token=446b2c9ddbb18da40dfbae172b2da18b30abf9027c0884e1cd6d4ffb3f6026e0
```

---

## Features

* **Live Streaming Markdown**: Markdown text and code blocks stream in real-time as the agent generates tokens.
* **Reactive Status Indicators**:
  * **Emerald**: Live and ready.
  * **Amber (Pulse)**: Agent is actively thinking or executing tools.
  * **Blue**: Initializing connection.
* **One-Click Stop / Cancellation**: When the agent is busy, a **Stop** button appears dynamically in the input bar. Clicking it dispatches `ClientFrame::CancelTurn`.
* **Interactive Tool Approvals**: When an agent requests permission (e.g. running a shell command), the web UI presents an approval modal with **Yes**, **No**, and custom input options.
* **Follower Mode**: If another client already holds the read-write lease, the web UI indicates `Follower (Read-Only)`.
