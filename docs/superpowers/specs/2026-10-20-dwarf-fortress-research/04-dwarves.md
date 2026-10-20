# 04 — Dwarves: attributes, skills, personality, needs, stress, moods, body

Part of the Dwarf Fortress research (2026-10-20); start at `00-synthesis.md`. Source tags as in
`01-world-and-embark.md`.

What a dwarf is for the player: a person with a name, a history, preferences and moods, who
does the work and whose happiness is the fortress's real resource. Mechanically: a creature
(defined by `CREATURE` raws, body from `BODY`/`TISSUE` templates) plus a mind (personality,
needs, emotions) plus a unit state machine (current job, path, carried items, wounds).

## 1. Attributes

[wiki Attribute] + [mem for the list]:

- **Physical (6)**: strength, agility, toughness, endurance, recuperation, disease resistance.
- **Mental (13)** [mem]: analytical ability, focus, willpower, creativity, intuition, patience,
  memory, linguistic ability, spatial sense, musicality, kinesthetic sense, empathy, social
  awareness.
- Range 0–5000. Human medians are 1000; species medians differ (dwarf agility median 900,
  "slightly below average"; dwarf toughness-related medians 1250 "higher than average").
  Descriptions change at ±250/500/750/1000 from the species median.
- Effects: strength → melee damage, muscle mass, carry capacity, fast-gait speed; agility →
  fast-gait speed; mental attributes feed skill quality rolls (below) and learning.
- Attributes improve with use and decay without it [mem].

## 2. Skills

[wiki Skill]:

- Levels: Dabbling (0), Novice (1) … Legendary (15+). "All skills take (400 + 100 × the new
  level) experience points to gain a level" — Novice 500, 18,000 total to Legendary. Combat
  skills keep climbing past Legendary; farming/gathering/fishing use an older formula that caps
  at Legendary+10.
- Unused skills go rusty, then very rusty, then decay.
- **Quality roll** for crafted items: `rand(0..10) + rand(0..level·5/2) + rand(0..level·5/2)`
  plus attribute terms `rand(0..phys1)/100 + rand(0..phys2)/250 + rand(0..phys3)/250` (same for
  mental), times luck; a matching material or item preference is "worth on average four skill
  levels". Masterwork chance is capped at 1/3 [wiki Item_quality].
- Skill also shortens job time: "Legendary skill can eliminate all time required to do a job
  down to a single action".
- Status ailments (stunned, dizzy, …) multiply skill down; two ailments compound (−75 %).
- Focus from needs (§4) gives ±50 % effective skill [wiki Personality_need].

## 3. Personality

- **Facets**: ~50 [mem], each 0–100, with seven report bands (0–9 lowest … 91–100 highest;
  40–60 neutral, 78 % of the population) [wiki Personality_facet]. Species shift medians
  (dwarves greed median 55). Facets decide which events give good or bad emotions, which social
  skills develop, and grudges (opposite facets > 60 vs < 40).
- **Beliefs/values** and **goals** [mem]: ~30 value axes (tradition, craftsmanship, family, …)
  and a life goal; conflict with facets or with other dwarves' values.
- **Preferences** [mem]: liked materials, items, creatures, foods, drinks, colours, shapes —
  drive good thoughts and the quality bonus above.

## 4. Needs and focus

[wiki Personality_need]:

- Each dwarf has a weighted set of needs derived from personality (pray to a deity, socialise,
  drink alcohol, eat a good meal, craft something, fight, learn, wander, be with family/friends,
  …; ~30 kinds [mem]).
- Unmet needs lower **focus**; met needs raise it. Focus is "the percentage of the combined
  totals of met over unmet need values": Very focused ≥140 %, Untroubled 100 %, Badly distracted
  ≤60 %; it scales effective skill up to ±50 %.
- Need satisfaction creates **personal jobs**: low priority (cancellable for fortress work) or
  high priority (magenta, never cancelled).
- Needs replaced the old "on break" system (classic ≤0.40 [mem]).

## 5. Thoughts, emotions, stress, breakdowns

[wiki Thought, Stress]:

- Events produce **emotions** with strength; the dwarf's facets decide which emotion an event
  gives (the same event can please one dwarf and upset another). Thoughts are listed on the
  dwarf's sheet; memories are relived later (short-term → long-term memory slots [mem]).
- **Stress** is two hidden integers (negative = good): short-term −100,000…100,000 (shown as the
  mood face, ecstatic → miserable) and long-term −50,000…120,000 (statuses: stressed +25,000,
  haggard +50,000, harrowed +100,000). Long-term stress moves slowly: at most +20,160 or
  −43,564 per year under ideal conditions.
- High stress → **breakdowns**: tantrums (break things, fight), depression, obliviousness; then
  long-term conditions and finally insanity (stark raving mad, melancholy → suicide, berserk)
  [wiki Stress, mem].
- Typical thought sources: eating in a good dining room, room quality, seeing a corpse, rain,
  a friend dying, a good meal, no alcohol, being hungry/thirsty/tired, talking with friends;
  meals give a thought if they match a preference or are worth ≥20☼ (thresholds 5/25/100/200/400☼)
  [wiki Thought].
- Leaders (expedition leader, mayor) absorb complaints: unhappy dwarves "yell at or cry on
  someone in charge" for relief [wiki Noble].
- This loop is the **tantrum spiral**: a death stresses friends, a tantrum hurts or kills
  someone, which stresses more friends. It is DF's canonical fortress-death mechanism
  (`11-fail-states-pacing-performance.md`).

## 6. Movement

[wiki Gait]: gait speed is **ticks per 100 tiles**; normal walking is **900 = 9 ticks per tile**;
the hard maximum is 1 tick per tile. Speed is modified by carried weight, skills (climbing,
swimming), attributes and personality. Climbing is slow (dwarf 5951), crawling slower. At DF's
100 FPS cap a walking dwarf crosses ~11 tiles per real second; this is DF's compressed time scale
(`11-…` §2).

## 7. Basic needs: eat, drink, sleep

All counters **increment by 1 per tick** [wiki Food, Sleep]:

| counter | considers (1/120 chance per tick, idle) | decides (idle) | status | worse | death / insanity |
|---|---|---|---|---|---|
| hunger | 40,000 | 45,000 | Hungry 50,000 | 65,000 unhappy thought, cancels job to eat; 75,000 Starving (hunts vermin); 85,000 thought | 100,000 burns fat, then starves |
| drowsiness | 50,000 | 54,000 | 65,000 Tired thought | 150,000 Very Drowsy; 160,000 Exhausted | 200,000 insane |
| thirst | 20,000 | 22,000 | Thirsty 25,000 | 35,000 unhappy thought, cancels job; 50,000 Dehydrated; 60,000 thought | 75,000 dies of thirst |

- Eating takes 50,000 off hunger (minimum 0); a drink takes 50,000 off thirst [wiki Thirst]. Sleeping lowers drowsiness by **19 per tick**; a
  sleep lasts ~2,650–2,900 ticks; dwarves spend ~5 % of their lives asleep. Pregnancy doubles
  hunger and raises drowsiness.
- Dwarves are `ALCOHOL_DEPENDENT`: they drink booze about five times a season and can live on
  water, but "dry" dwarves lose walking and attack speed (slowness thoughts after 3, 6, 9, 12
  months dry); repeating the same drink gives a bad thought; drinking without a mug/cup/goblet
  gives a bad thought [wiki Alcohol]. "Drink alcohol" is also a personality need.
- Sleeping in a bed (owned bedroom) vs on the floor gives different thoughts; sleeping in the
  rain or with noise is bad [mem].

Two observations for the engine: (1) the counters are pure functions of "tick of last
satisfaction" — they never need per-tick increments; (2) the "1/120 chance per tick" style is
pervasive in DF and is exactly what our stateless `slime::roll(tick, subject, salt)` gives.

## 8. Strange moods and artifacts

[wiki Strange_mood]:

- A fortress counter starts at 1000 and drops by 1 every 100 ticks (12×/day, ~3 months); at
  zero there is a 1-in-500 chance per check of a mood, capped by number of artifacts already
  made vs. (items created / 100) and (revealed subterranean tiles / 2304 — one 48×48 square).
- The moody dwarf is chosen by a weighted lottery (6 tickets per most professions, more for
  some), claims a workshop for their highest moodable skill, demands materials, and makes an
  **artifact**; completion grants 20,000 XP (Legendary). Waiting 50,000 ticks for a material
  drives them insane. Types: fey, secretive, possessed, fell, macabre.
- Artifacts are the largest single wealth items and enter history.

## 9. Relationships

[wiki Relationship]: friends, grudges, lovers, spouses, children, pets; deities with worship
levels (ardent/faithful/casual/dubious). Initial trust from the trust facet tier; love value
−100…100; talking to grudges can add 700+ stress a day.

## 10. Body and health

[wiki Combat] + [mem]:

- No hit points: bodies are trees of **body parts** (from `BODY` raws) with **tissue layers**
  (skin, fat, muscle, bone, nerve, artery, organs) of real materials (`TISSUE`/`MATERIAL`).
  Damage is applied per layer by attack type (edge/blunt, momentum vs. material yield/fracture
  values, `05-…` §3) and causes bleeding, pain, fractures, severed parts, organ failure,
  suffocation, infection; syndromes (venom, disease) layer effects on top.
- Health care: a hospital zone, chief medical dwarf, diagnosis → surgery, suturing, setting
  bones, casts, splints, crutches; traction benches; dwarves recover wounded to beds. Permanent
  injuries leave cripples. Starting supplies include splints and crutches [wiki Embark].

MVP-level reading: a *wound list* per body region (head, torso, arms, legs) with bleed and
pain is enough to make injuries, deaths and "rest in bed until healed" work; the full layer and
material model is a later upgrade that should keep the same API.

## 11. MVP tiers

| system | tier |
|---|---|
| Unit = creature raws + position + current job + inventory | **MVP** |
| Walking gait (ticks per tile) | **MVP** |
| Hunger, thirst, drowsiness counters with DF thresholds; eat, drink, sleep jobs | **MVP** |
| Skills with XP and level-based job speed | **MVP** (a few skills: mining, woodcutting, carpentry, masonry, farming, brewing, cooking) |
| Attributes | soon after (fixed medians in MVP) |
| Quality roll | soon after (with the first quality items) |
| Thoughts → short-term stress → mood face | soon after (a reduced table: hungry, thirsty, tired, ate/drank well, slept in bed, saw a corpse, friend died, room quality) |
| Personality facets, needs, focus | later (big tables; they make dwarves individuals) |
| Long-term stress, breakdowns, tantrum spirals | later (needs the above) |
| Relationships, families, deities | later |
| Strange moods + artifacts | later (iconic; needs workshops, materials, skills) |
| Body: wound list per region, bleeding, death | **MVP** (minimal, for the first threat) |
| Full tissue-layer body model, medicine | later |

## Sources

- https://dwarffortresswiki.org/index.php/Attribute
- https://dwarffortresswiki.org/index.php/Skill
- https://dwarffortresswiki.org/index.php/Item_quality
- https://dwarffortresswiki.org/index.php/Personality_facet
- https://dwarffortresswiki.org/index.php/Personality_need
- https://dwarffortresswiki.org/index.php/Thought
- https://dwarffortresswiki.org/index.php/Stress
- https://dwarffortresswiki.org/index.php/Strange_mood
- https://dwarffortresswiki.org/index.php/Relationship
- https://dwarffortresswiki.org/index.php/Food
- https://dwarffortresswiki.org/index.php/Sleep
- https://dwarffortresswiki.org/index.php/Alcohol
- https://dwarffortresswiki.org/index.php/Thirst
- https://dwarffortresswiki.org/index.php/Gait
- https://dwarffortresswiki.org/index.php/Combat
- https://dwarffortresswiki.org/index.php/Noble
