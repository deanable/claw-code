# Implementing Native Google AI Studio Support In `claw.exe`

This document describes the work required to make `claw.exe` support Google AI Studio / Gemini natively instead of routing through the existing OpenAI-compatible provider path.

## Problem Summary

Today the launcher can emit a Google AI Studio profile like:

```json
{
  "providerKind": "googleAiStudio",
  "model": "gemini-2.0-flash",
  "baseUrl": "https://generativelanguage.googleapis.com/v1"
}
```

But `claw.exe` does not have a real Google provider implementation. The runtime still uses the OpenAI-compatible client and constructs:

```text
{base_url}/chat/completions
```

That logic lives in:

- [rust/crates/api/src/providers/openai_compat.rs](C:/Users/Dean/source/repos/claw-code/rust/crates/api/src/providers/openai_compat.rs)

So a Google profile currently becomes something like:

```text
https://generativelanguage.googleapis.com/v1/chat/completions
```

which is not the intended Gemini API path for this codebase, and `claw.exe` exits after printing:

```text
api returned 404 Not Found
```

This is not a launcher crash. It is an unsupported provider path inside `claw.exe`.

## Current Relevant Code

Provider/client exports:

- [rust/crates/api/src/lib.rs](C:/Users/Dean/source/repos/claw-code/rust/crates/api/src/lib.rs)

OpenAI-compatible provider implementation:

- [rust/crates/api/src/providers/openai_compat.rs](C:/Users/Dean/source/repos/claw-code/rust/crates/api/src/providers/openai_compat.rs)

Launcher profile and env injection:

- [rust/crates/rusty-claude-cli/src/bin/claw-launcher.rs](C:/Users/Dean/source/repos/claw-code/rust/crates/rusty-claude-cli/src/bin/claw-launcher.rs)

Runtime config parsing:

- [rust/crates/runtime/src/config.rs](C:/Users/Dean/source/repos/claw-code/rust/crates/runtime/src/config.rs)

## Goal

Add first-class Google AI Studio support so that:

1. `claw.exe` can recognize a Google/Gemini provider mode.
2. It sends requests to Google-native endpoints instead of `/chat/completions`.
3. It maps Gemini request/response payloads into the existing internal `MessageRequest`, `MessageResponse`, and stream event abstractions.
4. Tool use remains supported where Gemini supports it.
5. The launcher can safely re-enable Google launches once `claw.exe` has native support.

## Recommended Architecture

Do not try to keep Google on the OpenAI-compatible client.

Instead, add a dedicated provider implementation alongside the existing providers:

- `anthropic`
- `openai_compat`
- new: `google_ai_studio`

Recommended new file:

- `rust/crates/api/src/providers/google_ai_studio.rs`

## Implementation Steps

### 1. Add a dedicated Google provider module

Create a new provider module:

- `rust/crates/api/src/providers/google_ai_studio.rs`

It should mirror the role of `openai_compat.rs`, but speak Gemini-native HTTP.

Suggested public types:

- `GoogleAiStudioConfig`
- `GoogleAiStudioClient`

Suggested env vars:

- `GOOGLE_API_KEY`
- `GOOGLE_BASE_URL`

Suggested default base URL:

- `https://generativelanguage.googleapis.com/v1beta`

Notes:

- Prefer `v1beta` unless the rest of the implementation is intentionally pinned to a newer stable Google endpoint.
- Keep the base URL overrideable the same way other providers already are.

### 2. Extend provider selection

Locate provider detection and model/provider resolution in the `api` crate and extend them with a Google-specific variant.

Expected areas:

- `rust/crates/api/src/providers/mod.rs`
- any `ProviderKind` enum or provider detection helpers re-exported by [rust/crates/api/src/lib.rs](C:/Users/Dean/source/repos/claw-code/rust/crates/api/src/lib.rs)

Tasks:

- Add a new provider kind for Google AI Studio / Gemini.
- Ensure provider selection can resolve to `GoogleAiStudioClient` instead of `OpenAiCompatClient`.
- Keep existing OpenAI-compatible providers unchanged.

### 3. Define the Google request mapping

Map the internal request model to Gemini-native payloads.

Input source types already used by the app:

- `MessageRequest`
- `InputMessage`
- `InputContentBlock`
- tool definitions

Gemini-native concepts you will need to map:

- conversation contents
- system instruction
- generation config
- tools / function declarations
- tool call responses

Recommended strategy:

- Create explicit conversion helpers from internal types to Google request JSON.
- Do not intermingle Google-specific shape logic into the generic OpenAI-compatible file.

Important mapping details:

- Internal text blocks should map to Gemini text parts.
- Tool definitions should map to Gemini function declarations.
- Tool results should map to the corresponding function response parts.
- Preserve token limits and tool-choice behavior as closely as possible.

### 4. Implement non-streaming responses first

Before tackling streaming, get the basic non-streaming round trip working.

Suggested initial endpoint:

- `POST {base_url}/models/{model}:generateContent?key={api_key}`

Tasks:

- Send a minimal prompt with no tools.
- Parse candidate content back into internal `MessageResponse`.
- Convert Gemini finish reasons into the internal finish semantics already used by the runtime.

This is the fastest path to proving the provider works.

### 5. Add streaming support

After non-streaming works, implement streaming in the provider using the Gemini streaming endpoint.

Suggested endpoint family:

- `:streamGenerateContent`

Requirements:

- Translate provider chunks into the internal stream event model already used by the CLI.
- Preserve text deltas.
- Emit tool-call start / delta / stop events if Gemini supplies them in incremental form.
- Ensure end-of-message and usage semantics remain consistent with other providers.

If Gemini streaming shape is materially different, it is acceptable to:

1. implement non-streaming first,
2. gate Google streaming behind a feature flag or temporary fallback,
3. then finish streaming parity.

### 6. Add Google-specific auth and headers

Do not use bearer auth if the Google endpoint expects API key query params or Google-specific header conventions.

Implementation details to confirm in code:

- whether the API key belongs in `?key=...`
- whether `x-goog-api-key` is preferable for some endpoints
- whether any content-type or API-version headers are required

Keep this logic local to `google_ai_studio.rs`.

### 7. Wire config and env resolution

The launcher currently injects:

- `OPENAI_API_KEY`
- `OPENAI_BASE_URL`

That must change once `claw.exe` supports Google natively.

Tasks:

- Update the launcher to inject Google-specific env vars when `providerKind == GoogleAiStudio`.
- Update runtime/provider resolution so `claw.exe` reads `GOOGLE_API_KEY` and `GOOGLE_BASE_URL` for that provider.
- Avoid overloading `OPENAI_BASE_URL` for Google.

Launcher file to update:

- [rust/crates/rusty-claude-cli/src/bin/claw-launcher.rs](C:/Users/Dean/source/repos/claw-code/rust/crates/rusty-claude-cli/src/bin/claw-launcher.rs)

### 8. Add model catalog logic for Gemini

The launcher work already narrowed visible Gemini models, but `claw.exe` should also understand Google model aliases and defaults.

Tasks:

- Add a Google default model, likely `gemini-2.0-flash`.
- Add token window metadata for known Gemini models.
- Ensure max-output logic does not incorrectly apply OpenAI-specific assumptions.

### 9. Handle tool-use feature parity carefully

This is the highest-risk integration surface.

Checklist:

- Verify Gemini function declaration schema matches internal tool schema requirements.
- Verify multiple tool calls in a single turn.
- Verify tool result round-tripping.
- Verify models that do not support tools are rejected or downgraded cleanly.

Do not assume parity just because the model supports “function calling” in principle.

### 10. Improve error handling for unsupported provider/base URL combinations

Even after native support lands, add preflight checks so the user gets a useful message instead of a raw 404.

Recommended safeguards:

- If provider kind is Google and base URL is obviously OpenAI-style, emit a config error.
- If provider kind is OpenAI-compatible and base URL is Google-native, emit a config error.
- Include the final resolved provider kind and base URL in debug output when available.

## Suggested File-Level Work Plan

### `rust/crates/api/src/providers/google_ai_studio.rs`

Implement:

- config struct
- env reading
- request builders
- response parsers
- streaming adapter
- error translation

### `rust/crates/api/src/providers/mod.rs`

Update:

- provider kind enum
- provider detection
- factory wiring

### `rust/crates/api/src/lib.rs`

Update exports:

- export `GoogleAiStudioClient`
- export config if needed

### `rust/crates/rusty-claude-cli/src/bin/claw-launcher.rs`

After native support exists:

- restore Google launch support
- inject `GOOGLE_API_KEY` / `GOOGLE_BASE_URL`
- keep provider-specific model filtering

### `rust/crates/runtime/...`

Update any provider resolution or config plumbing that still assumes Google is OpenAI-compatible.

## Testing Plan

### Unit tests

Add focused tests for:

- request URL construction
- API key placement
- conversion of internal messages to Gemini JSON
- conversion of Gemini responses to internal message objects
- tool declaration mapping
- tool result mapping

### Integration tests

Add provider-specific integration tests similar in spirit to existing API integration tests.

Recommended coverage:

1. simple prompt round trip
2. streaming text round trip
3. tool invocation round trip
4. provider error translation
5. bad model / bad endpoint diagnostics

### Manual verification

After implementation:

1. Build:
   - `cd rust`
   - `cargo build -p rusty-claude-cli --bin claw --release`
2. Set Google env vars:
   - `GOOGLE_API_KEY`
   - optional `GOOGLE_BASE_URL`
3. Run:
   - `claw --model gemini-2.0-flash prompt "hello"`
4. Verify:
   - no `/chat/completions` requests are used
   - no `404 Not Found` on launch
   - response text renders normally
5. Test tools:
   - `claw --model gemini-2.0-flash`
   - ask for a file read or grep in a small workspace

## Rollout Strategy

Recommended sequence:

1. Add provider kind and non-streaming Google client.
2. Add a minimal happy-path integration test.
3. Wire launcher env vars for Google.
4. Add streaming.
5. Add tool-use support and tests.
6. Re-enable Google launch support in the launcher.

This order keeps the debugging surface manageable.

## Practical Warning

The risky part is not the launcher UI. The risky part is preserving internal event semantics while adapting Gemini request/response shapes into the existing provider abstraction. Keep the provider-specific translation isolated, and avoid “just enough” hacks inside `openai_compat.rs`. That file should remain OpenAI-compatible only.
