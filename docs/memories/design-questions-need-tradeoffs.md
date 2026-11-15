---
name: design-questions-need-tradeoffs
description: "When asking Dawid design questions, spell out the trade-offs in prose first — he rejects option prompts that arrive without the reasoning"
metadata: 
  node_type: memory
  type: feedback
  originSessionId: 1ae6ac93-ffef-467c-9ba6-ec5347743da7
---

During brainstorming/design sessions, Dawid twice interrupted multiple-choice questions (coordinate systems, tick pacing) to ask "what are the tradeoffs".

**Why:** He decides from the trade-off analysis, not from option labels; short option descriptions aren't enough for architecture choices.

**How to apply:** For any non-trivial design decision, first write a prose section laying out each option's trade-offs along the relevant axes (and which axes *don't* matter), state a recommendation with the reason, and only then present the AskUserQuestion choices.
