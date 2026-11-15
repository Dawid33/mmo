---
name: commander-camera-controls
description: "User's rulings on commander camera controls (2026-10-31): no zoom, no wheel speed tiers, constant speed, Space/Shift = straight vertical, sinks underground"
metadata:
  node_type: memory
  type: feedback
  originSessionId: 23c96439-19a9-4ffc-a931-8100fde894fa
  modified: 2026-10-31T11:40:50.258Z
---

Commander camera (2026-10-31): "Hide above" (cut on) stays as it was — level stepping with Space/Shift, camera on the level floor, wheel zoom; never change it. Free camera (cut off): no zoom; Space/Shift move the eye STRAIGHT up/down (not along the view ray — that reads as zoom); pan at one constant speed; held Shift sinks through the surface into a free underground camera (x-ray of dug spaces). User called "mouse wheel picks the speed tier" a terrible decision — don't propose it again.

2026-11-02 user: the free camera must NOT follow the terrain, the cut or trees (no lazy terrain band, no tree clearance), must rise arbitrarily high (old cap was ZOOM_FAR 150 m), and the level must not change when toggling Hide above. Now: absolute focus height, level = focus's level in both modes.

**Why:** zoom-scaled speed made speed/height a hidden, misjudged state; wheel speed tiers felt wrong.
**How to apply:** keep camera state visible and direct; ask before giving the wheel a new job. Related: [[fortress-ui-landed]].

2026-10-31 (uncommitted): the underground look (no sun shadows, brighter ambient, dark rock backdrop cubemap + close fog instead of the skybox) applies ONLY while the eye is inside a solid block (`commander::underground_view` / `eye_in_block`, home tile mirror; dug air/leaves/water = open air). User: "If I am no longer inside a block I should not see the underground."
