---
name: debugging-follow-user-evidence
description: "When the user points at a specific cause, add logging and follow THAT evidence — don't spin off multiple theories/redesigns"
metadata: 
  node_type: memory
  type: feedback
  originSessionId: 5191e9d6-591a-4df5-8035-13db82287c56
---

On the reconcile/runaway bug the user got frustrated: "you're going in weird
directions constantly." I had cycled through several hypotheses (clock lead,
throughput, home-only-prediction redesign) and even executed a whole
multi-agent implementation, when the user had told me early the cause was
**player inputs not getting reconciled**.

**Why:** the user knows this codebase far better than I infer it from a few
reads; my layered theorizing burned effort and drifted from their signal.

**How to apply:** when the user names a specific suspected cause, treat it as
the working hypothesis. Add targeted logging / instrumentation at that spot and
let the empirical output drive the next step — do NOT branch into alternative
architectures or big redesigns unless the evidence at their named spot rules it
out. Prefer "add logging, run, read output" over multi-hypothesis prose. Keep
responses short and action-first when they're frustrated. See
[[client-prediction-runaway]].
