# Custom-model output overrides

Discovery metadata and explicit output limits are separate. A `/models` refresh can update a model's discovered context and output ceiling; it does not erase an explicit override. Existing non-null discovery values are not automatically classified as explicit owner intent.

Authenticated account-scoped API:

- `GET /api/providers/{id}/output-overrides` returns the current `provider_revision`, independent override `revision`, saved `overrides` and current model `limits`.
- `PUT /api/providers/{id}/output-overrides` requires exactly these four fields:

```json
{"model":"exact/model-id","max_output_tokens":262144,"provider_revision":"value from GET","revision":"value from GET"}
```

For a provider without an override record, its override revision is the empty string. Reload after any provider or override revision error. Discovery refreshes do not change the override revision. Two writes using the same revision cannot both succeed.

`max_output_tokens` must be an integer from 1 to 10000000, or **null to clear** that model's explicit value. A missing field is rejected. Strings, fractions, booleans and out-of-range values are rejected. A clear gets a new revision even if the record is empty, so stale requests cannot undo it.

The model ID is exact and case-sensitive. New values require a currently discovered model. If a model disappears, its saved value remains dormant; it cannot create or revive a catalogue model. A dormant value can still be cleared. If the exact model reappears at the same provider endpoint, its value applies again.

The saved intent may exceed the currently discovered context. The effective limit is clamped to that context without replacing the saved intent. `limits.discovered_max_output_tokens` remains raw discovery metadata. `override_max_output_tokens` identifies explicit intent; `effective_max_output_tokens` is its context-clamped value, or the discovered value when there is no override. If both are null, the existing custom-provider harness default applies: the smaller of 32768 and half the context. Null never means unlimited output. The actual request also subtracts estimated input and its context reserve; other transport and response limits remain unchanged.

Provider renaming and credential rotation at the same endpoint preserve overrides. Endpoint replacement or deletion clears that provider's override record in the same SQLite transaction as its configuration update. Recreating a deleted provider creates a new provider revision; stale requests cannot restore the old record. Other providers are unaffected. Workspace transfer carries explicit records with validated provider/endpoint bindings; older packages without them import an empty explicit store.

The limits are captured when a new custom-provider run starts. Changing a setting does not replay, restart or modify an already running model session. Hosted providers and custom providers without an override keep their existing behavior.
