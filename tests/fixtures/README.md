# Synthetic reasoning SSE regression fixtures

These fixtures are hand-authored synthetic streams, **not real provider captures**.
No real capture was available. They model the installed `async-openai 0.41.3`
contracts in `src/types/responses/stream.rs` (event tags, required sequence/item/
output/part indices and delta/text fields), with output item shapes from
`src/types/responses/response.rs`. They do not establish compatibility with any
particular provider or claim that these event sequences were observed in production.

Runner tests serve these bytes using `spawn_mock_responses`, consume the actual
`Client::responses().create_stream` decoder, and fold decoded events through
`PartialResponse`; no UI is involved. Each stream read has a five-second timeout.

- `reasoning-mixed.sse`: interleaved raw reasoning and summary deltas; done events
  replace drafts; a final reasoning item omits `content` and has empty `summary`,
  requiring preservation of both streamed fields.
- `reasoning-done-only.sse`: done events initialize absent content and a new part
  without any preceding delta or content-part event.
- `reasoning-both-fields.sse`: an item containing both content and summary survives
  SDK decoding, folding and an `OutputItem` serialization/deserialization roundtrip.
- `reasoning-invalid-targets.sse`: raw delta/done events at `u32::MAX` must not
  allocate content; events aimed at a message instead of reasoning are ignored.
- `reasoning-malformed.sse`: intentionally omits the required delta field; the SDK
  stream must emit a decoding error, not silently ignore the event.

Successful streams end with a minimal `response.completed` event. Its empty output
is deliberate: assertions check accumulated events rather than a completed-response
snapshot concealing lost deltas. The malformed stream is consumed only through its
first error.

Run: `cargo test reasoning_sse_ -- --nocapture`
