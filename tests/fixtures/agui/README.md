# AG-UI conformance fixtures

Payloads our AG-UI types are tested against. Vendored rather than fetched, so
the suite stays hermetic and keeps answering the same question after the
specification moves.

## Pinned source

`ag-ui-protocol/ag-ui`, commit **`e42bdbe`**, `@ag-ui/core` **0.0.58**.

The pin is not decoration. AG-UI is pre-1.0 and moving — that repository
merged a PR the day before this material was taken. Re-fetching later gets a
different HEAD, and a test that started failing would then be ambiguous
between *our types drifted* and *the specification moved*. Vendoring is what
removes the ambiguity; the pin is what lets someone reproduce the fetch.

## Two sources, and neither substitutes for the other

**Do not delete one of these as redundant.** They look alike and they test
different things.

- **Spec-derived** (`null-omission.json`, `usage.json`) tests whether we read
  the **specification** right: field names, what is optional, what is an array,
  what a producer omits rather than nulls.
- **Provider-shaped** (`provider-shaped.json`) tests whether a **real stream
  fits inside what the specification allows**: escape-heavy deltas, non-ASCII
  in ordinary text, id lengths, the material a schema permits but no fixture
  author thinks to write.

A worked example of why both: `ag-ui-core` 0.1.0 types `MessageId`, `ThreadId`
and `RunId` as UUID newtypes where the specification says plain strings. Both
sources catch it — the spec's own `msg-1` and a provider's
`msg_abc123` fail identically. **An earlier draft of this file claimed only
the provider payload would catch it. That was tested and it was false**, and
the false version is recorded here because a justification nobody can check is
worth less than the rule it defends. What the provider set uniquely catches is
narrower and duller: length and encoding assumptions that a hand-written
fixture rounds off.

`ToolCallId` is a plain `String` newtype even in `ag-ui-core`, so `tool-1`
parses there. The defect is narrower than "its id types are UUIDs".

## Provenance, in three tiers

Weakest last. **Do not grant a lower tier the authority of a higher one.**

1. **Published fixture** — `null-omission.json`. Copied byte for byte from
   `sdks/fixtures/`. This is the specification's own data, written to be
   shared across SDKs. 28 cases, and despite its name it covers nearly every
   event type rather than only the null contract.
2. **Transcribed from spec test source** — `usage.json`. The payloads are the
   specification's, out of
   `sdks/typescript/packages/proto/__tests__/usage-roundtrip.test.ts`. The
   JSON encoding is ours, because the originals are TypeScript object
   literals. **A transcription carries our transcription bugs.**
3. **Constructed by us** — `provider-shaped.json`. The AG-UI shapes are ours;
   no adapter exists yet to emit a real conforming stream. The ids and text
   inside them were observed on the wire.

**A gap worth knowing about:** the specification's test corpus contains **no
`REASONING_*` payloads at all** beyond a single `REASONING_MESSAGE_CHUNK` case
in `null-omission.json`. For the one event family we have measured as
load-bearing, tier 3 is not a shortcut — it is the only tier available.

## Licences

MIT throughout, and **the notice differs by where the file came from**, so
each vendored file needs the one that matches its source.

| File | Notice | Holder |
|---|---|---|
| `null-omission.json` | `LICENSE.ag-ui-root` | none named — the notice reads `Copyright (c) 2025` |
| `usage.json` | `LICENSE.ag-ui-typescript` | Tawkit Inc.; Markus Ecker |
| `provider-shaped.json` | none | ours; nothing is copied from the AG-UI repository |

`sdks/fixtures/` has no nearer `LICENSE`, so the repository root one governs
it, and that notice **names no copyright holder**. It is reproduced verbatim.
Do not "fix" it by inventing one — writing "AG-UI contributors" would be
attributing to a party the licence does not name.
