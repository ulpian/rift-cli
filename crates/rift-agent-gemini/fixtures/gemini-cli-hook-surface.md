# PINNED EVIDENCE ARTIFACT — Gemini CLI's own published hook surface

This file is a **fixture**, not documentation. It pins the hook surface Gemini
CLI itself publishes, with repository, version, path, blob sha and a verbatim
excerpt, so the Gemini rows of
`crates/rift-shadow-hooks/tests/adapter_sweep.rs` are measured against the
agent rather than against a third party's view of it. Every field below is
asserted by the `pinned_surface` tests in `crates/rift-agent-gemini/src/lib.rs`.

The bytes are the published npm package, not the TypeScript sources: the
package is what a user runs, and a reviewer reproduces each sha offline with
`npm pack @google/gemini-cli-core@0.35.3` and `git hash-object` over the named
path inside the tarball.

## What this pin overturned

The sweep recorded that Gemini states its model only on `BeforeModel`, so a
session the adapter captured had no model. That was true of a Go wrapper that
parsed only `BeforeModel`'s payload and false of the agent: Artifact A's
`AfterModelInput` carries the same `llm_request`, and Artifact B shows the
handler filling it on every `AfterModel` it fires. The adapter maps
`AfterModel` to a response event and reads `llm_request.model` on any verb, so
the model reaches the projection on the response.

## Artifact A — the hook types

    repository:     https://github.com/google-gemini/gemini-cli
    package:        @google/gemini-cli-core
    tag:            0.35.3
    path:           dist/src/hooks/types.d.ts
    blob-sha:       07e68ea295b682a53300863fb06057061e10ea76

The event enumeration, verbatim:

```typescript
export declare enum HookEventName {
    BeforeTool = "BeforeTool",
    AfterTool = "AfterTool",
    BeforeAgent = "BeforeAgent",
    Notification = "Notification",
    AfterAgent = "AfterAgent",
    SessionStart = "SessionStart",
    SessionEnd = "SessionEnd",
    PreCompress = "PreCompress",
    BeforeModel = "BeforeModel",
    AfterModel = "AfterModel",
    BeforeToolSelection = "BeforeToolSelection"
}
```

The inputs of the four events this pin decides, verbatim:

```typescript
export declare enum NotificationType {
    ToolPermission = "ToolPermission"
}
export interface NotificationInput extends HookInput {
    notification_type: NotificationType;
    message: string;
    details: Record<string, unknown>;
}
export interface BeforeModelInput extends HookInput {
    llm_request: LLMRequest;
}
export interface AfterModelInput extends HookInput {
    llm_request: LLMRequest;
    llm_response: LLMResponse;
}
export interface BeforeToolSelectionInput extends HookInput {
    llm_request: LLMRequest;
}
```

## Artifact B — the handler that fills `AfterModel`

    repository:     https://github.com/google-gemini/gemini-cli
    package:        @google/gemini-cli-core
    tag:            0.35.3
    path:           dist/src/hooks/hookEventHandler.js
    blob-sha:       b118b97674dd67ad72b7668e0237a0a71cb514f6

```javascript
    async fireAfterModelEvent(llmRequest, llmResponse) {
        const input = {
            ...this.createBaseInput(HookEventName.AfterModel),
            llm_request: defaultHookTranslator.toHookLLMRequest(llmRequest),
            llm_response: defaultHookTranslator.toHookLLMResponse(llmResponse),
        };
        return this.executeHooks(HookEventName.AfterModel, input, undefined, llmRequest);
    }
```

## Artifact C — the translator that names the model

    repository:     https://github.com/google-gemini/gemini-cli
    package:        @google/gemini-cli-core
    tag:            0.35.3
    path:           dist/src/hooks/hookTranslator.js
    blob-sha:       bae7987a02e6fde5f86c18cdc611ba0dee825adc

```javascript
        return {
            model: sdkRequest.model || DEFAULT_GEMINI_FLASH_MODEL,
```
