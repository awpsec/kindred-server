# Local model image support

Custom provider catalogues may return only model IDs. Kindred recognizes the
verified vision-capable `incoai/Qwen3.6-35B-A3B-Splash` ID (and its unqualified
form) when vision metadata is absent. Explicit false flags or text-only input
modalities take precedence. Unknown model IDs remain text-only.

Reference: https://huggingface.co/incoai/Qwen3.6-35B-A3B-Splash

The Pi bridge preserves PNG, JPEG, WebP and GIF MIME types from read_attachment
and other image-returning tools, subject to its existing image size limit.
Previously this bridge accepted PNG only.

After installing this server update, refresh the custom provider's model
catalogue in Connections before retrying an existing attachment. These changes
require the server and packaged Pi harness; a desktop-only update is insufficient.

Verification uses the actual Pi SDK with synthetic HTTP responses to assert
outgoing image payloads, plus server catalogue tests. It does not establish
connectivity or actual vision inference on the owner's Mac.
