# 04 — Cozy / non-violent / social multiplayer games

Research date: 2026-09-20. Scope: shipped non-violent or low-violence multiplayer games, read
for what they teach a strictly non-violent voxel MMO whose world is a set of offline-generated,
finite, persistent "super regions" (8–16 km, one server process each) joined by loading screens.

Conventions: source URLs inline. `[unverified]` = from my own recollection or a secondary
summary I could not confirm against a fetched source. Fetch gaps are listed at the end.

---

## 1. Per-game notes

Each entry answers: (1) structure, (2) transition, (3) purpose, (4) other players, (5) cadence,
(6) what worked / failed.

### Animal Crossing: New Horizons (ACNH)

1. **Structure.** One persistent, personal, fully editable island per console (a sore point:
   the second user on a shared Switch is a second-class resident —
   https://gamerant.com/animal-crossing-new-horizons-fix-one-island-problem-review-bombed/).
   Up to 8 players on an island online (owner + 7; 12 on Switch 2 —
   https://nookipedia.com/wiki/Multiplayer). Mystery Islands are disposable: "randomly generated
   from templates each time they are visited and deleted once the player leaves"; ~20 templates
   with their own flora, fish, bugs (https://nookipedia.com/wiki/Mystery_Island_Tour,
   https://wuffs.org/acnh/mysterytour.html). Dream Suite publishes a *read-only copy* of an
   island that anyone can visit alone, while the owner is offline, with no vandalism risk
   (https://nookipedia.com/wiki/Dream_Suite).
2. **Transition.** Everything goes through the airport (Dodo Airlines): a diegetic counter, a
   seaplane cutscene, a loading screen. Mystery tours cost a Nook Miles Ticket (2,000 miles — a
   currency earned by ordinary play). Visiting a person needs their gate open: friends-only,
   best-friends-only, or a 5-character one-session **Dodo Code**. Visitors carry their pockets
   (that is what makes inter-island trading possible) [unverified in a fetched source;
   well-known].
3. **Purpose.** Self-expression and collection: decorate the island, fill the museum, complete
   the catalogue, earn a 5-star rating, unlock terraforming. No win state; K.K. Slider's concert
   is a soft "credits".
4. **Other players.** Islands differ on purpose (native fruit, shop stock, turnip price,
   hemisphere/season), so *other people's places have things yours lacks*. The turnip "stalk
   market" gives each island a different, twice-daily price, which spawned an entire
   out-of-game institution: Turnip.Exchange queues, hosts charging entry fees/tips, 13-hour
   lines, queue bots, scams (https://nookipedia.com/wiki/Community:Turnip.Exchange,
   https://www.inverse.com/gaming/animal-crossing-app-stalk-market-turnip-exchange-bots).
   Anti-grief is permission-tiered: ordinary visitors cannot use shovels/axes; only "Best
   Friends" can (https://nookipedia.com/wiki/Multiplayer).
5. **Cadence.** Real-time clock, real seasons, shops with hours, a short list of dailies (rocks,
   fossils, villagers). Progress is deliberately months long; impatient players "time travel"
   by changing the system clock (https://screenrant.com/animal-crossing-new-horizons-time-travel-problems-controversy/).
6. **Worked / failed.** Worked: asymmetry between islands made visiting *economically*
   meaningful, and the ticketed random island is a perfect low-stakes "expedition" loop. Failed:
   the transition is too heavy — every arrival freezes everyone on the island for a long
   unskippable cutscene (https://www.vice.com/en/article/animal-crossing-has-an-unskippable-cutscene-that-is-making-me-hate-my-visitors/,
   https://www.gamesradar.com/i-love-animal-crossing-new-horizons-multiplayer-but-something-has-got-to-be-done-about-those-loading-screens/);
   matchmaking/trade was left entirely to third-party sites, which is where the scams lived.

### Glitch (Tiny Speck, 2009–2012)

1. **Structure.** 2D browser MMO set inside the minds of eleven Giants. The world was a graph of
   side-scrolling **streets** grouped into regions, each street a separately loaded location
   (the released location data is "XML representations of the construction of every street in
   Ur" — https://github.com/tinyspeck/glitch-locations). Players later got personal, decorable
   **home streets** that generated "imagination" when opened to visitors
   (https://tropedia.fandom.com/wiki/Glitch_(video_game), via search summary). The shared world
   *grew through player activity*: Butterfield — "There's a natural limit in the size of the
   road that is being expanded through the activities of players"
   (https://www.gamedeveloper.com/business/nibbling-pigs-in-giants-dreams-tiny-speck-s-butterfield-talks-i-glitch-i-).
   Community "street projects" where players pooled items and labour to open a new street, and
   late-game server-wide "feats", are [unverified] — I recall them but found no fetchable
   source.
2. **Transition.** Walk to a signpost at the end of a street → brief load → next street; subway
   and teleportation for long hops [partly unverified; Grokipedia summary only].
3. **Purpose.** Skills learned in real time (continuing offline), 500+ achievements, quests,
   donating to Giants' shrines, housing, a player economy in "currants"
   (https://www.qualbert.com/glitch-mmo/). Explicitly no win: "there was no way to win the game.
   And because of that, you couldn't lose"
   (https://www.johnnyrodgers.is/the-death-of-glitch-the-birth-of-slack/).
4. **Other players.** Group bonuses on gathering: "mining with other players gets it done a lot
   faster and gives people bonuses" (Butterfield, link above). Players named animals and chose
   which trees grew on a street. Volunteer **greeters** met every newcomer on a one-time
   greeting street. Moderation was theatrical but humane: a disruptive player was pulled into a
   black room with a single lamp where staff tried de-escalation before any ban
   (https://en.wikipedia.org/wiki/Glitch_(video_game)).
5. **Cadence.** Energy and mood meters that drained with activity; seasonal holidays
   ("Zilloween"); offline skill timers as a gentle reason to return.
6. **Worked / failed** (postmortem: https://www.gamedeveloper.com/business/the-story-of-i-glitch-i-why-this-odd-mmo-is-shutting-down).
   - *Illegible purpose.* "The game was too foreign of a concept for most people"; newcomers
     said "I don't know what the fuck I'm supposed to do", and most closed the tab. The tutorial
     fix came a year too late.
   - *Fun arrived late.* "It took us until the final four months for the game to actually
     become really fun from a moment-to-moment gameplay point of view."
   - *Veterans ran out.* The Nov 2011 "unlaunch" was to fix onboarding *and* give invested
     players tools to build locations, design buildings and form organisations
     (https://gamesbeat.com/tiny-speck-unlaunches-its-glitch-game-taking-it-down-to-rework-game-play/).
   - *Platform.* "We shouldn't've done it in Flash" — no path to tablets.
   - *Cost.* A ~40-person team and a hand-authored world, ~$17M raised, ~150k peak players
     [Grokipedia figures, unverified]; a loved niche that could not pay for bespoke content.
   - Worked: the community. Thousands attended the End of the World party; all art and code
     went public-domain, feeding three fan remakes (Eleven, Children of Ur, Odd Giants).

### Sky: Children of the Light (and Journey)

1. **Structure.** A Home hub island plus seven authored realms "representing different stages
   of life", each a chain of areas; Aviary Village as a social/shop hub
   (https://en.wikipedia.org/wiki/Sky:_Children_of_the_Light). Areas are small shared instances
   of strangers plus your friends (roughly 8 per area [unverified]). Nothing is generated or
   player-built (shared spaces came much later and are limited).
2. **Transition.** You *fly* through a gate in the clouds — the load is hidden inside the
   game's core verb. Progress (winged light, cosmetics, friends) carries; nothing is gated by
   tickets.
3. **Purpose.** Find spirits, relive their memories, buy their emotes/cosmetics with candles;
   a ritual endgame (Eye of Eden) where you give your winged light away and are reborn with
   ascended candles.
4. **Other players.** The best-documented prosocial design in the cluster (GDC 2020 summary:
   https://www.pockettactics.com/sky-children-of-the-light/friends). Strangers are grey
   silhouettes until you offer a candle; friendship is a tree of unlocks each costing candles
   (name → hold hands → hug → chat). Text chat with strangers exists only at hard-to-reach
   benches, so "players must invest before accessing communication tools". Hand-holding lets a
   veteran literally lead a newcomer. **Hearts** can only be *received* from others. The
   economy was a response to playtests: without cosmetics "players impersonated each other and
   stole gifts". Journey's antecedents: two anonymous players, no names, no text, musical
   calls; avatar collision removed because blocking paths was a form of conflict; shared
   resources re-designed so simultaneous pickup never disappoints; the team role-played
   griefers internally to find exploits
   (https://www.gamedeveloper.com/design/cutting-corners-networking-design-in-journey,
   https://www.gamedeveloper.com/audio/gdc-2012-how-i-journey-i-was-designed-to-facilitate-friendship —
   "a single rule can pollute an entire system").
5. **Cadence.** Daily candle cap (~20/day), daily quests, ~quarterly seasons with a paid pass,
   and "Traveling Spirits" re-running old seasons at higher prices.
6. **Worked / failed.** 50M+ installs by Oct 2020; genuinely kind community. Failed: the
   candle run became a job — players report 1–3 h/day routes, "boring, mundane, empty-hearted
   grind", "extreme FOMO" (https://steamcommunity.com/app/2325290/discussions/0/3873717500287767981/).
   A cozy theme on a scarcity economy.

### Palia

1. **Structure.** Two (now more) shared adventure zones — Kilima Valley hub, Bahari Bay — each
   a shared server instance; every player's **housing plot is a private instance** behind a
   gate in the zone (https://palia.wiki.gg/wiki/Housing_Plot). Authored, not generated.
2. **Transition.** Zone gates / fast-travel boards with a load; plots are opt-in to visit and
   the owner chooses what visitors may use (https://paliaguide.com/multiplayer).
3. **Purpose.** Eight life skills, villager friendships/romance, house building, collections.
4. **Other players.** Nodes are **not contested**: everyone who hits a node gets full loot.
   **Flow trees** regenerate faster than one player can chop, so they need 2+ players, and
   everyone who helps gets the rare wood. That single rule produced a server-wide norm: call
   out rare finds in chat, fire a flare arrow, wait a moment for others
   (https://palia.wiki.gg/wiki/Guide:Etiquette, https://gamerant.com/palia-unwritten-rules-flow-trees-no-loot-stealing/).
   Proximity auras give +XP for fishing/hunting near others; a request board lets you gift
   items others asked for.
5. **Cadence.** Short in-game day cycle, weekly villager gift limits, rotating events.
6. **Worked / failed.** Worked: "positive-sum nodes" is the cleanest cooperation incentive in
   the cluster. Failed: thin MMO depth and weak true co-op (players could not co-decorate a plot
   a year in), monetisation complaints, two layoff rounds (49, then 36 staff) and sale to
   Daybreak in 2024 — though the game survived and passed 10M players by April 2026
   (https://en.wikipedia.org/wiki/Palia).

### Book of Travels / Meadow (Might and Delight)

1. **Structure.** BoT: a hand-painted land (Braided Shore) of connected scenes; servers
   deliberately capped to a handful of players so that meetings are rare and memorable. Meadow:
   one open landscape, players as animals.
2. **Transition.** Walking between scenes, hand-pumped rail carts (BoT).
3. **Purpose.** BoT declared "no overarching goals, beginnings, or ends"
   (https://en.wikipedia.org/wiki/Book_of_Travels). Meadow: collect flowers/shards, unlock
   animals; obelisks are easier in a pack.
4. **Other players.** No text or voice: emotes/symbols only. Might and Delight's argument is
   that removing chat removes verbal abuse and forces good-faith reading of strangers; a Meadow
   player "made more friends in three days … than in their whole life"
   (https://www.killscreen.com/key-pleasant-online-game-communities-get-rid-chat/).
5. **Cadence.** None imposed.
6. **Worked / failed.** Meadow (small, cheap, 2016) is still fondly played. BoT launched into
   early access Oct 2021 after four delays, missed its targets, and the studio cut from 35 to
   10 people within two months (https://massivelyop.com/2021/12/21/book-of-travels-just-had-serious-layoffs-as-might-and-delight-cuts-team-from-35-to-10/);
   servers went offline 31 July 2026 with the studio citing an "unsustainable" process and not
   having "the capacity required for this type of project". Lesson: *wandering with no goals
   plus a tiny population plus hand-authored content* gives neither a reason to return nor
   anyone to meet when you do.

### One Hour One Life (OHOL)

1–3. One huge shared procedural map; you live 60 minutes, born as a helpless baby to another
   player, and die of old age. The purpose is inheritance: you are building for people you
   will never meet, inside a village someone else left you. Crafting is done visibly on the
   ground so knowledge spreads by watching
   (https://www.gamedeveloper.com/design/making-a-civilization-scale-crafting-system-for-jason-rohrer-s-i-one-hour-one-life-i-).
4–6. Rohrer uses hard scarcity so that "players have to come together and have a culture around
   well usage". Not strictly non-violent (murder exists) and griefing is severe: "one
   experienced griefer can just about destroy a town that took dozens of players dozens of
   hours". The countermeasure is social: players **curse** offenders; enough curses exile you
   to distant "Donkey Town"; curses can be **forgiven**
   (https://onehouronelife.fandom.com/wiki/Curse). Family trees are browsable on the web — a
   strong asynchronous trace.

### Minecraft peaceful/community servers (Hermitcraft model)

Whitelisted vanilla SMPs converge on the same institutions: a central **shopping district**
(Hermitcraft S7: on a mushroom island at world origin), a **diamond** commodity currency, honour
-system shops, a town hall, and self-imposed land rules — e.g. one diamond block per 10×10 of
shop footprint, proposed by a player and adopted by consensus
(https://hermitcraft.fandom.com/wiki/Season_7_Shopping_District). Retention comes from *seasons*
(fresh world every year or so, with a world download/tour of the old one), build contests,
community megaprojects and running jokes. Grief prevention is mostly social (whitelist,
reputation) plus rollback tools [unverified, general knowledge].

### Eco

No combat at all; a shared finite world with a simulated ecosystem and a meteor due in ~30
days. Skill-point costs scale with a server "collaboration" setting so nobody can be
self-sufficient, and players write **laws** that become hard physical limits (e.g. 10 trees per
player per day) (https://wiki.play.eco/en/Collaboration,
https://developland.strangeloopgames.com/the-design-pillars-of-eco/). Purpose comes from a
shared deadline — useful as a *per-region, opt-in event* rather than our whole tone.

### Spiritfarer, Stardew, Dinkum, Core Keeper (brief)

Spiritfarer structures a whole game as *a home that travels*: the boat is the persistent
base, islands are destinations, the voyage itself is when you cook, farm and talk
(https://thunderlotusgames.com/games/spiritfarer/). Its purpose is caretaking and letting go —
completion states that are emotional, not numerical. Stardew/Dinkum/Core Keeper co-op is
"one shared homestead, friends only"; the relevant lesson is only that shared-ownership
permissions (who may move my furniture) are the first thing co-op players ask for — the same
gap Palia was criticised for.

---

## 2. Principles from cozy / prosocial design writing

**Project Horseshoe "Cozy Games" (Cook, Forbes, Howe et al., 2017)** —
https://lostgarden.com/2018/01/24/cozy-games/
- Coziness = the fantasy of **safety, abundance, softness**. Abundance matters most for us:
  when food and shelter are solved, players turn to belonging, beauty and mastery.
- Negated by: extrinsic/transactional reward pressure, mandatory responsibilities, FOMO,
  non-consensual social presence, "vast unknowable spaces".
- Wants: intrinsically pleasant verbs, breadth of *optional* activities, "safe rituals"
  (hands busy, mind free), ritual/seasonal rhythm without urgency.
- Social: opt-in layers (stranger → acquaintance → friend), call-and-response broadcasts,
  persistent small cohorts, gifts that show personal effort, "Mend, don't end" moderation.
- Watch for emergent min-maxing that turns a cozy system into a job.

**Cook, "Game design patterns for building friendships"** —
https://lostgarden.com/2017/01/27/game-design-patterns-for-building-friendships/
- Friendship needs **proximity** (repeated unplanned encounters with recognisable people),
  **similarity**, **reciprocity** (escalating exchange loops), **disclosure** (safe quiet
  time).
- Killers: stranger matchmaking with no reunion, huge anonymous servers ("disposable people"),
  large power gaps, too many modes thinning density.
- Numbers: cohorts around **150**; a session should contain **30 %+ familiar faces**.

**Koster & Cammarata, "The Trust Spectrum"** — https://www.raphkoster.com/2018/03/16/the-trust-spectrum/
- Mechanics imply a trust level. Low: parallel play, symmetric abilities, verified exchange.
  Medium: soft overlapping roles, shared metrics. High: exclusive roles, blind hand-offs,
  shared failure.
- Games cannot force people up the ladder; offer a *ramp* where the same activity works alone
  together, and works better with trusted partners.

**thatgamecompany** — shape behaviour by removing the *affordance* for harm (collision,
contested pickups, free chat) rather than by policing it; make every escalation of intimacy
cost a little and be mutual.

---

## 3. Transferable ideas for a non-violent voxel MMO

1. **One diegetic travel institution.** ACNH's airport and Sky's cloud gates show a loading
   screen is forgiven when it is a *place and a verb*. Pick one (harbour + ship, airship dock,
   rail terminus) and build a terminal in every super region. Keep the arrival cheap: never
   freeze the players already there (ACNH's worst mistake).
2. **Make regions asymmetric on purpose.** Offline generation with geology/climate gives each
   region native stone, timber, dyes, crops and fauna — ACNH's native-fruit trick at continental
   scale. Trade and travel then have a reason without any scarcity inside a region.
3. **Ticketed expedition regions.** Alongside persistent regions, mint disposable or
   long-rotating "survey" regions reached by a ticket earned in ordinary play (Nook Miles
   Ticket). The unlimited offline budget means these can be far richer than ACNH's 20 templates.
4. **Read-only "dream" visits.** Publish a frozen snapshot of a region/claim that anyone can
   tour alone or with friends, with edits discarded (Dream Suite). With deterministic sim and
   serialised regions this is nearly free, works for browser drop-ins, and is grief-proof —
   also the natural form for world tours and build-contest judging.
5. **Positive-sum resource nodes.** Everyone who works a node gets full yield (Palia); a few
   rare nodes *require* two or more pairs of hands (flow trees; Glitch's faster group mining).
   Add a flare/beacon item so calling others is a game verb, not only chat.
6. **Community works that open the map.** Glitch's player-expanded road: a new bridge,
   tunnel, lighthouse or *the route to a newly generated region* opens when a region's
   residents pool materials and labour. Answers "why am I doing this?" with a visible,
   permanent, named result.
7. **Generated history as the quest-giver.** Sky's spirits are memories found in the landscape.
   If offline generation produces ruins, old roads and named events, then surveying, restoring
   and cataloguing them (a regional museum à la Blathers, filled collectively) is a non-violent
   completion track unique to each region.
8. **Tiered build permissions.** Visitor (look, use doors, trade) → friend (use stations,
   harvest) → best friend (place/break) — ACNH's shovel rule plus Palia's per-plot toggles.
   Ship co-editing permissions early; every co-op cozy game was asked for them first.
9. **Remove harm affordances rather than police them.** No player collision blocking doors,
   no contested drops, claims for edits, and cheap per-claim rollback (the undo log already
   exists). Follow Journey's method: spend a playtest trying to ruin each other's day.
10. **Friendship as an unlock ladder with stakes.** Sky's candle-lit reveal and paid mutual
    unlocks (name, lead-by-hand, chat, teleport-to). Leading a newcomer by the hand across 2 km
    of terrain is a strong fit for a big seamless region.
11. **Greeters and humane moderation.** Glitch's volunteer greeters on an arrival street and
    its "black room" de-escalation; OHOL's forgivable curses; Horseshoe's "mend, don't end".
12. **Home cohorts of ~150.** Give each player a home region/settlement and bias instancing and
    arrival points so ≥30 % of faces are familiar (Cook). A super region should feel like a
    county with villages, not a lobby.
13. **A market district per region, with local prices.** Hermitcraft's shopping district as a
    supported institution (stall plots, honour-box shops, land fee paid to a town fund), plus
    ACNH's per-place price differences — but build the price board and travel queue *in-game*
    so Turnip.Exchange-style scams and bots have no gap to fill.
14. **Seasons as world vintages, not battle passes.** Hermitcraft-style: periodically a new
    set of regions is generated and opened with ceremony; old ones stay, become tour/dream
    destinations, and are never deleted. Returning content should not be priced up (Sky's
    Traveling Spirits resentment).
15. **Inheritance traces.** OHOL's browsable family trees → per-region chronicles: who founded
    this village, who built this bridge, guest books, signed plaques. Asynchronous presence
    makes a quiet server feel inhabited (BoT's fatal emptiness).
16. **A home that travels.** Spiritfarer's boat: let the vehicle used for region travel be a
    small personal buildable space, so the loading screen is bracketed by time in your own
    cosy interior and you arrive *with* something of yours.
17. **Opt-in regional "great works" with deadlines.** Eco's meteor, scoped down: a festival,
    a flood season, a comet viewing — time-boxed, per region, skippable, with laws/charters
    voted by residents as hard limits (tree quotas in a protected forest).
18. **Wordless layer for strangers.** Emotes, music, lanterns and hand-holding as the default
    stranger channel (Meadow, Journey), text for friends and for market/beacon broadcasts. It
    also sidesteps much of browser-client chat moderation.

---

## 4. Warnings

- **Illegible purpose kills onboarding (Glitch).** "No combat, do anything" read as "nothing
  to do". The first ten minutes need one legible, genre-familiar goal (build a shelter, reach
  the harbour) before whimsy. Glitch fixed this a year too late.
- **Hand-authored content cannot feed an MMO on a niche audience (Glitch, BoT, Palia).** All
  three burned money on bespoke content and laid off. Our generator and the players' own
  building must be the content pipeline.
- **No goals + tiny population = empty (BoT).** Rare encounters are magical only if they
  happen. Guarantee density (arrival hubs, scheduled gatherings, elastic instancing) and leave
  asynchronous traces for when they do not.
- **Cozy theme on a scarcity economy becomes a second job (Sky).** Daily caps tuned to a paid
  shortcut produce 1–3 h "runs" and FOMO. Horseshoe: extrinsic pressure negates coziness. If
  there is a daily rhythm, make it a 10-minute ritual whose absence costs nothing.
- **Real-time gates invite clock cheating and resentment (ACNH).** Server time prevents the
  cheat, not the resentment.
- **Heavy transitions poison visiting (ACNH).** A loading screen between regions is fine; a
  ceremony that interrupts *other* players is not.
- **If you do not build the trade/visit matchmaking, a third party will — with scams, bots and
  entry fees (Turnip.Exchange).**
- **"Vast unknowable spaces" are explicitly un-cozy (Horseshoe).** 8–16 km regions with 2 km
  views need legibility: landmarks, roads, maps, waystations, and enclosed warm places to
  return to. Contemplative-exploratory and cozy pull against each other; the "cozy sandwich"
  (safe hearth ↔ long walk) reconciles them.
- **One determined griefer outweighs dozens of builders (OHOL).** In a persistent editable
  world, claims + permission tiers + rollback must exist before the first public test; social
  sanctions alone rotate accounts.
- **Forced intimacy backfires (Koster, Horseshoe).** Require co-operation only for optional
  bonuses; never gate core progression behind finding a partner.
- **Platform lock-in (Glitch/Flash).** Already mitigated by browser + native, but keep the
  input model touch-plausible.

---

## 5. Gaps

- 403/402/truncated: PC Gamer (BoT shutdown; Jenova Chen "nobody is born toxic" interview),
  MassivelyOP (BoT sunset, 2026-08-04), Fanlore, TV Tropes and Tropedia (Glitch). Not worked
  around. Consequently Glitch's street-project/feat mechanics, subway, and Rook attacks are
  `[unverified]`, as are BoT's exact per-server player cap and Sky's per-area player count.
- No primary GDC Vault material was fetched for Sky (used Pocket Tactics' write-up of the GDC
  2020 talk) or for Cook's friendship talk (used the Lost Garden essay).
- Dinkum, Core Keeper, Disney Dreamlight Valley and Wylde Flowers were judged not to add
  anything beyond the entries above and were not researched.
