---
name: hwahap
description: "Plan code implementation through decision rounds, or build an explicitly authorized contract. Use for implementation planning, code changes, tests and draft PR review. Handle general questions and documentation edits directly."
---

# Hwahap

Optimize total successful task cost. Follow the MCP server's `instructions` as the execution protocol.
Call `hwahap_step` with the repository path and the same stable `host_session_id` throughout.

For a planning request, start `request` with `plan_only:true` and finish at `plan_ready`.
After the user explicitly requests BUILD, pass the full stored plan digest to `build_confirmed`.
An ordinary implementation `request` uses `plan_only:false` and continues after plan confirmation.
For an already approved Codex plan, use `approved_plan` with its original request and approval reference
to bind the approval and plan digests to the executable translation. Preserve existing implementation
approval while recovering a missing `plan.json` through that handoff.
Keep source approval, exact draft replacement digest and independent translation reviews distinct.
When the user explicitly skips planning, use `build` with their verbatim authorization.
For repairs under unchanged contracts use `adjust_build`; contract changes reopen PLAN.

Use `question_batch` and `question_response` for decision rounds.
Keep questions to one sentence and outcomes in options; show supporting detail through plan links.
Keep progress updates brief and present parallel work units in a compact table.
Select an actually callable Codex question tool under its current mode and input limits.
See [USAGE.md](../../USAGE.md) for UI adaptation. Record answers from actual submitted responses;
keep defaults, cancellation and timeout in a waiting state. Forward user messages verbatim.
Deliver `CONFIRM PLAN` and `SHIP` as the exact lines typed by the user in a separate message.

Follow `next`; use event waits for native work and CI. Read artifact detail only when needed.
Let the host own Plan/Goal lifecycle; attach optional references through `host_context`.
Perform edits, tests, spawning and publication within the assigned dispatch and its authorization.
Use the bound model/effort decision and retain worker identities and roles. Workers perform their tasks directly.
Use `hwahap_status` for progress and `recheck_pr:true` for the current draft's review recovery.
Label host-reported answers, requested models and usage with their observation source.

[ARCHITECTURE.md](../../ARCHITECTURE.md) covers setup and execution structure.
[OPERATIONS.md](../../OPERATIONS.md) covers workflows and recovery.
[USAGE.md](../../USAGE.md) covers request shapes, question UI and host-side usage metering.
