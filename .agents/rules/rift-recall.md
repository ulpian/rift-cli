<!-- RIFT-MANAGED:antigravity:begin -->
Rift automatically captures this repository and delivers bounded recall context through native hooks.
- `rift agent-help` — inspect the live recall and recovery surface.
- `rift recall status --json` — verify hook, runtime, context, and degradation status.
Documents (.docx, .pdf, .xlsx and the like) are converted once for this repository and kept as Markdown under `.rift/state/doc/`; a hook points a document read there, or names the file to read in its place. Read that Markdown rather than the binary, or ask `rift recall doc-read <path>` for it; where there is none, read the document as usual.
If Rift reports degradation, let the raw agent call run and use the diagnostic above; correctness never depends on this file.
<!-- RIFT-MANAGED:antigravity:end -->
