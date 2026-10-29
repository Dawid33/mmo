# Dwarf Fortress creatures — full roster for picking game creatures

Research only (2026-10-29): nothing was added to the game. This lists every creature DF
defines, so we can pick which ones to model. It feeds the animal and threat tiers in
`../2026-10-20-dwarf-fortress-research/06-agriculture-and-food.md` §4 and `10-threats-and-military.md`.

**Source.** Taken from the game's own data, not the wiki: the creature raws of the locally
installed DF **0.47.04** (`/usr/share/games/dwarf-fortress/gamedata/raw/objects/creature_*.txt`,
31 files, 767 `[CREATURE:]` entries), parsed with a throwaway script. The Steam (v50+) roster is
largely the same; any creatures added after 0.47 are **not** here. Every entry is in
[`creatures.csv`](creatures.csv) (id, name, category, source file, adult body size, raw biome
tokens, flag tokens, products, population, cavern depth, raw variations, DF description), so it
can be filtered without re-parsing.

**Units.** DF body size is in cm³. ≈ grams of water, so `70 kg` below means `BODY_SIZE 70000`.
Habitat is a summary of the raw `BIOME` tokens. "any land" = `ANY_LAND`/`ALL_MAIN`; "—" =
no biome, i.e. it only appears with civilizations (domestic stock) or through world history.

## Counts

| Category | # | What it is |
|---|---:|---|
| Civilization races | 5 | dwarf, elf, human, goblin, kobold. They own entities (civs). |
| Domestic | 23 | `COMMON_DOMESTIC`: livestock and pets that civs bring and trade. |
| Megabeasts | 4 | `MEGABEAST`: unique world-history monsters that attack rich fortresses. |
| Semi-megabeasts | 4 | `SEMIMEGABEAST`: smaller unique monsters. |
| Fantastic surface | 28 | Monsters and fey of savage/good/evil biomes, sea monsters, gnomes, odd vermin. |
| Underground | 67 | Anything with a `SUBTERRANEAN_*` biome: the three cavern layers and the magma sea, including cavern humanoids ("X man"). |
| Wild surface animals | 178 | Real-world animals large enough to be full units. |
| Vermin | 92 | `VERMIN_*`: tiny ambient creatures (insects, small birds/fish/rodents). Not full units in DF; caught in traps, hunted by cats. |
| Giant animals | 181 | `GIANT_*` variants of the real animals, savage biomes only. Mechanically "same animal, bigger, `SAVAGE`". |
| Animal people | 181 | `X man` humanoid variants of the animals, savage biomes, intelligent, can join civs. |
| Placeholders | 4 | centaur, chimera, griffon (`DOES_NOT_EXIST`, kept for legends) and the wagon (a merchant cart DF models as a creature). |
| **Total** | **767** | |

Giants and animal people make up half the roster but are *derived* (DF stamps them out with
`COPY_TAGS_FROM` + a creature variation). If we want them, do it the same way: a modifier on a
base creature, not 362 separate definitions.

## Not in the raws (procedural)

DF generates these per world, so no list can be complete. Each needs its own generator:

- **Forgotten beasts** (cavern-dwelling, one random body + breath/syndrome each), **titans**
  (surface), **demons** (the circus / underworld, many kinds) and **unique demons**.
- **Night creatures**: night trolls, bogeymen, vampires, **werebeasts** (cursed, full moon),
  husks/thralls from evil-weather syndromes.
- **Necromancers' undead** and their **experiments**. Also **divine** creatures (angels),
  and the procedural **gods**.

## Which DF flags matter for a game

What each tag drives in DF, which tells us the behaviour we'd need to build per creature:

- **Roles**: `PET`/`PET_EXOTIC` (tameable), `TRAINABLE(_WAR|_HUNTING)` (war/hunting animal),
  `MOUNT`, `PACK_ANIMAL`, `WAGON_PULLER`, `GRAZER` (needs a pasture, eats grass), and the
  products `MILKABLE`, `LAYS_EGGS`, shearable wool/hair (`SHEARABLE_TISSUE_LAYER`), silk (`WEBBER`).
- **Threat**: `LARGE_PREDATOR` (attacks units), `AMBUSHPREDATOR`, `BONECARN`,
  `CURIOUSBEAST_ITEM|EATER|GUZZLER` (thieves: steal items/food/drink, e.g. gremlins),
  `MEGABEAST`/`SEMIMEGABEAST` (scheduled attacks by fortress wealth).
- **Spawning**: `BIOME:*`, `POPULATION_NUMBER`, `FREQUENCY`, `UNDERGROUND_DEPTH` (cavern layer 1–3,
  4 = magma sea, 5 = underworld), `LARGE_ROAMING` (wanders on and off the map), `VERMIN_*` (ambient
  swarm, not a unit), and the alignment flags `GOOD`/`EVIL`/`SAVAGE`, which limit a creature to
  biomes of that alignment.

## Candidate shortlist (suggestion)

Picked against the existing tiers (MVP = one tame animal + one hostile wildlife type;
`EntityKind` currently has only `Player` and `Slime`). These are suggestions to discuss, not
decisions:

1. **MVP**: *dog* or *cat* (tame, follows / hunts vermin) + *wolf*, *black bear* or *cougar*
   (one hostile temperate `LARGE_PREDATOR`). Keep the slime as ambient wildlife.
2. **Livestock / pastures**: *cow* (milk), *sheep* (wool), *chicken* (eggs, nest box), *goat*,
   *pig*, *donkey*/*horse* (pack/mount). These six cover all the product chains.
3. **Huntable wildlife**: *deer*, *elk*, *wild boar*, *hare*, *fox*, *badger*, plus fish
   (*carp*, *salmon*, *pike*) for fishing.
4. **Vermin layer**: *rat*, *fly*, *beetle*, small birds. Fine as a cheap non-unit particle/ambient
   system, which is how DF treats them.
5. **Caverns**: *giant cave spider* (webs, silk), *crundle*, *cave crocodile*, *troglodyte*,
   *cave dragon*, *blind cave ogre*, *gremlin* (thief), *giant olm*. Plus *magma crab* / *fire imp*
   for the magma sea. They fit the implied-ground underground work.
6. **Big threats later**: *dragon*, *hydra*, *roc*, *bronze colossus* (megabeasts), *giant*,
   *cyclops*, *ettin*, *minotaur* (semi-megabeasts). Forgotten beasts as a procedural generator.

## Civilization races

| Creature | Adult size | Habitat | Traits |
|---|---|---|---|
| dwarf | 60 kg | — | intelligent |
| elf | 60 kg | — | intelligent |
| goblin | 60 kg | — | evil biome, intelligent |
| human | 70 kg | — | intelligent |
| kobold | 20 kg | — |  |

## Domestic animals

| Creature | Adult size | Habitat | Traits | Products |
|---|---|---|---|---|
| alpaca | 70 kg | — | domestic, pet | milk wool/hair |
| beak dog | 150 kg | wetland | domestic, pet, mount, predator, evil biome | eggs |
| blue peafowl | 4.0 kg | forest, (tropical) | domestic, pet | eggs |
| cat | 5.0 kg | — | domestic, pet |  |
| cavy | 0.8 kg | grass/savanna, (tropical) | domestic, pet |  |
| chicken | 3.0 kg | — | domestic, pet | eggs |
| cow | 600 kg | — | domestic, pet | milk |
| dog | 30 kg | — | domestic, pet, trainable |  |
| donkey | 300 kg | — | domestic, pet, pack | milk |
| duck | 1.0 kg | fresh/brackish water, wetland | domestic, pet, flies | eggs |
| goat | 50 kg | — | domestic, pet | milk |
| goose | 4.5 kg | fresh/brackish water, wetland, (temperate) | domestic, pet, flies | eggs |
| guineafowl | 1.5 kg | — | domestic, pet | eggs |
| horse | 500 kg | grass/savanna, (temperate) | domestic, pet, pack, wagon, mount | milk |
| llama | 180 kg | — | domestic, pet | milk wool/hair |
| mule | 400 kg | — | domestic, pet, pack |  |
| pig | 60 kg | — | domestic, pet | milk |
| rabbit | 0.5 kg | grass/savanna, (temperate) | domestic, pet |  |
| reindeer | 130 kg | cold | domestic, pet | milk |
| sheep | 50 kg | — | domestic, pet | milk wool/hair |
| turkey | 5.0 kg | wetland, forest, grass/savanna, (temperate) | domestic, pet | eggs |
| water buffalo | 1,000 kg | wetland, (tropical) | domestic, pet, pack, wagon | milk |
| yak | 700 kg | mountain | domestic, pet, pack, wagon | milk |

## Megabeasts

| Creature | Adult size | Habitat | Traits | DF description |
|---|---|---|---|---|
| bronze colossus | 20,000 kg | any land | predator, not living, megabeast | A gigantic magic statue made of bronze and bent on mayhem. |
| dragon | 25,000 kg | any land | exotic pet, trainable, exotic mount, predator, steals items, megabeast | A gigantic reptilian creature.  It is magical and can breathe fire.  These monsters can live for thousands of years. |
| hydra | 8,000 kg | any land | exotic pet, predator, megabeast | A giant dragon-like monster with seven biting heads. |
| roc | 20,000 kg | mountain | exotic pet, trainable, exotic mount, predator, flies, steals items, steals food, megabeast | A bird of prey so large and ferocious it dwarfs many dragons.  All beneath its mighty wings should fear the sky. |

## Semi-megabeasts

| Creature | Adult size | Habitat | Traits | DF description |
|---|---|---|---|---|
| cyclops | 8,000 kg | any land | predator, speaks, semi-megabeast | A giant humanoid monster with a single eye set in its forehead. |
| ettin | 8,000 kg | any land | predator, steals items, steals food, steals drink, speaks, semi-megabeast | A giant humanoid monster with two heads. |
| giant | 9,000 kg | any land | predator, steals items, steals food, steals drink, speaks, semi-megabeast | A gigantic creature resembling a human, almost unparalleled in size. |
| minotaur | 220 kg | any land | predator, speaks, semi-megabeast | A giant humanoid monster with the head of a bull. |

## Fantastic surface creatures

| Creature | Adult size | Habitat | Traits | DF description |
|---|---|---|---|---|
| acorn fly | 20 g | fresh/brackish water | flies, savage biome, vermin | An insect many times the size of its peers.  It is known for its deafening buzz. |
| blizzard man | 300 kg | cold | predator, evil biome | A large humanoid monster from the wild tundra.  It has translucent skin, icicles for teeth, red glowing eyes and pointed ears. |
| blood gnat | 1 g | fresh/brackish water | flies, evil biome, vermin | A tiny insect with a pulsating, lumpy body.  It is an evil bug that seeks blood. |
| dark gnome | 15 kg | mountain | evil biome, steals drink | A tiny, foul-tempered humanoid creature that dwells in the evil mountains.  They are known to enjoy drinking liquor and will take any unguarded supplies of booze. |
| demon rat | 0.3 kg | — | exotic pet, evil biome, gnaws, vermin | An insidious form of vermin.  It lives only to steal food from others. |
| fairy | 0.1 kg | any land | flies, good biome, intelligent, vermin | A tiny, giggling humanoid with lacy wings. |
| fluffy wambler | 2.0 kg | any land | exotic pet, good biome, vermin | A fluffy, pudge-filled being, known for its warm heart and stumble bumblings. |
| foul blendec | 60 kg | forest | predator, evil biome, speaks | A man-shaped creature with the legs of a goat and the empty-eyed skull of a goat. |
| fox squirrel | 2.0 kg | forest, (temperate) | exotic pet, savage biome, vermin | A small mammal noted for its tenacity.  It has long ears and flashing eyes. |
| grimeling | 70 kg | wetland | predator, amphibious, evil biome | An evil monster from the swamp.  It resembles a knot of waterlogged weeds but will strike the unaware victim. |
| harpy | 60 kg | wetland, grass/savanna | predator, flies, evil biome, speaks | A monster in the shape of a woman with a bird's wings in place of arms and talons for feet. |
| ice wolf | 50 kg | cold | predator, evil biome | A fierce creature from the evil snowy wilds.  It has white fur and a piercing howl. |
| knuckle worm | 1.0 kg | — | exotic pet, evil biome, vermin | A tiny creature made up of a series of crackling knobs set at strange angles. |
| merperson | 70 kg | ocean | aquatic, good biome, speaks | A man-like creature with the tail of a fish instead of legs. |
| moghopper | 0.3 kg | fresh/brackish water | exotic pet, amphibious, savage biome, fishable, vermin | A small mud-dwelling amphibian. |
| mountain gnome | 15 kg | mountain | good biome, steals drink | A tiny, jolly humanoid creature that dwells in the fair mountains.  They are known to enjoy drinking liquor and don't care whether it's theirs or not. |
| nightwing | 120 kg | desert | predator, flies, evil biome | A flying monster with stretched skin over its emaciated body.  It has the head of a jackal with needle-like horns protruding through its mane. |
| ogre | 6,000 kg | grass/savanna | predator, evil biome | A giant humanoid monster found stomping about in the evil plains.  Their low howls can be heard long before they are seen. |
| phantom spider | 0.5 kg | forest | exotic pet, evil biome, webs, vermin | A tiny translucent creature, found in evil forests. |
| pixie | 1 g | any land | flies, good biome, intelligent, vermin | A tiny winged humanoid with shifting colors which performs intricate aerial dances in large groups. |
| sasquatch | 300 kg | forest, cold, (temperate) | predator, savage biome | A large and mysterious ape-like creature found in temperate forests. |
| satyr | 60 kg | forest | good biome, speaks | A medium-sized humanoid with the horns and legs of a goat. |
| sea monster | 8,000 kg | ocean | predator, aquatic, evil biome | A giant creature with many eyes and arms to terrify the sea. |
| sea serpent | 9,000 kg | ocean | exotic pet, predator, aquatic, savage biome | A giant limbless dragon that lives in the sea. |
| strangler | 40 kg | forest, (tropical) | predator, evil biome | A large, four-armed ape creature with three eyes and razor-sharp teeth. |
| two-legged rhino lizard | 1.0 kg | any land | exotic pet, savage biome, gnaws, vermin | A tiny reptile, running on two legs.  It has a horn on the end of its nose. |
| unicorn | 600 kg | forest, grass/savanna, cold | pet, mount, good biome | A horse-like creature with a spiral horn growing from its forehead. |
| yeti | 300 kg | cold, mountain | predator, savage biome | A large ape-like creature with white fur, found in the snowy wilds. |

## Underground (caverns and magma sea)

| Creature | Adult size | Cavern layer | Habitat | Traits | DF description |
|---|---|---|---|---|---|
| amethyst man | 70 kg | 3-3 | cavern | predator, not living | A strange crystalline creature the shape of a man.  It is found deep underground. |
| amphibian man | 20 kg | 1-3 | cavern water | predator, amphibious, speaks | These evil creatures resemble walking frogs with arms and the intelligence to use them.  They live in the waters far under the earth. |
| antman | 20 kg | 1-3 | cavern | predator, flies, speaks | A humanoid with the head and body of an ant. |
| bat | 0.1 kg | 1-2 | cavern | exotic pet, flies, gnaws, vermin | A tiny winged creature that hunts at night and rests in caves during the day. |
| bat man | 0.1 kg |  | cavern | flies, gnaws | A humanoid with the head of a bat and wings stretching from wrist to ankle. |
| blind cave bear | 200 kg | 1-2 | cavern | exotic pet, exotic mount, predator | A huge emaciated-looking bear with great drooping ears and many sharp teeth.  It is found deep underground. |
| blind cave ogre | 7,000 kg | 2-3 | cavern | predator, evil biome | A large cavern-dwelling humanoid monster.  It has a gaping mouth with many sharp teeth.  It has no eyes and only two digits on each hand and foot. |
| blood man | 70 kg | 3-3 | cavern | predator, evil biome, not living | A man-shaped abomination made entirely of blood.  These cursed creatures are only found very near the underworld. |
| bugbat | 10 kg | 2-3 | cavern | exotic pet, flies | A small bat-like creature with the head of an insect.  It is found deep underground. |
| cap hopper | 0.2 kg | 1-2 | cavern water | exotic pet, amphibious, vermin | A tiny amphibian with a long prehensile tail.  It lives in underground swamps and tower-cap forests on which it feeds. |
| cave blob | 20 kg | 3-3 | cavern water, cavern | exotic pet | A strange shapeless life form with an orange leathery skin containing a liquid interior.  It can secrete its internal fluid. |
| cave crocodile | 600 kg | 1-2 | cavern water | exotic pet, exotic mount, predator, amphibious | A huge, predatory reptile with pale, colorless scales and red eyes.  It lives in caves and ambushes its prey. |
| cave dragon | 15,000 kg | 3-3 | cavern | exotic pet, trainable, predator, evil biome | A gigantic monster, once a dragon, now adapted to and polluted by the underground.  Its wings fall limp at its side.  Its face is full of incredibly long teeth.  Its eyes are large to penetrate the darkness. |
| cave fish | 1.0 kg | 1-2 | cavern water | aquatic, fishable, vermin | A small pale creature found in underground streams and ponds. |
| cave fish man | 1.0 kg |  | cavern water | aquatic, fishable | A humanoid creature with the head of a blind cave fish.  They live in colonies in watery regions far underground. |
| cave floater | 40 kg | 2-3 | cavern | flies | A floating pod with eye-stalks.  It can spray poison and it gives off poison gas when punctured. |
| cave lobster | 0.6 kg | 1-2 | cavern water | aquatic, fishable, vermin | A small, pale arthropod found in underground streams and ponds. |
| cave spider | 50 g | 1-2 | cavern water, cavern | exotic pet, webs, vermin | A tiny underground bug, sought after for its thread. |
| cave swallow | 0.1 kg | 1-2 | cavern | exotic pet, flies, vermin | A small blue and orange bird. |
| cave swallow man | 0.1 kg |  | cavern | flies | A feathered man with the wings and head of a bird.  It lives underground. |
| creeping eye | 20 kg | 3-3 | cavern | evil biome | A small underground monster that crawls across the cavern wall with its four clawed hands.  It has a single large eye which can shine with its own light, otherwise its stony skin blends in with the rock.  It has no mouth and is said to feed on evil alone. |
| creepy crawler | 1.0 kg | 3-3 | cavern | exotic pet, evil biome, vermin | A tiny underground creature made of a mass of appendages resembling human fingers.  It creeps across the ground like a starfish and eats with a mouth on the bottom of its body. |
| crundle | 10 kg | 2-3 | cavern water, cavern | exotic pet | A tiny underground monster with large claws and horns.  It walks on two legs and is dangerous when encountered in large numbers. |
| draltha | 2,500 kg | 1-2 | cavern | exotic pet | A large, long-bodied grazer with a thick mane that feeds on the tops of towercap mushrooms deep under the earth. |
| drunian | 50 kg | 1-2 | cavern | exotic pet, steals items, steals food | A large quadruped with a mane circling its manlike face and hands at the end of its forelimbs.  It lives underground and is fond of raiding the supplies of cavern outposts. |
| elk bird | 100 kg | 1-3 | cavern | exotic pet, exotic mount | A large creature found grazing on mushrooms deep underground.  It walks on two legs and has the head of a bird with the antlers of a great elk. |
| fire imp | 6.0 kg | 0-4 | magma sea | predator, fire immune | A small humanoid surrounded by fire which they can hurl at their enemies. |
| fire man | 70 kg | 3-4 | magma sea | predator, not living, fire immune | Fire in the shape of a human that can hurl fireballs. |
| fire snake | 1.0 kg | 2-4 | magma sea | exotic pet, fire immune, vermin | A serpent made of pure fire which can inject liquid fire as venom. |
| flesh ball | 70 kg | 3-3 | cavern water |  | A large ball of skin found moving around the debris near underground ponds.  It is there that it absorbs dead matter for food. |
| floating guts | 20 kg | 2-3 | cavern | exotic pet | A transparent and amorphous monster that lives underground.  It is small in size and is found crawling across the cavern floor.  Its organs appear to be floating inside of its body. |
| gabbro man | 70 kg | 3-3 | cavern | predator, not living | Living gabbro in the shape of a man.  These strange creatures are found deep underground. |
| giant bat | 200 kg | 1-2 | cavern | exotic pet, hunt-trainable, exotic mount, predator, flies | A giant flying mammal found underground. |
| giant cave spider | 200 kg | 1-2 | cavern | exotic pet, predator, ambusher, savage biome, webs | A large underground monster with eight legs and sharp, venomous fangs. |
| giant cave swallow | 200 kg | 1-2 | cavern | exotic pet, hunt-trainable, exotic mount, flies | A gigantic blue and orange bird. |
| giant cave toad | 200 kg | 1-2 | cavern water | exotic pet, exotic mount, predator, amphibious | A giant amphibian predator found underground. |
| giant earthworm | 200 kg | 1-2 | cavern |  | A gigantic version of its tiny cousin, this long, slimy creature tunnels through the rocks deep underground. |
| giant mole | 200 kg | 1-2 | cavern | exotic pet, steals food, steals drink | A gigantic digging creature found underground. |
| giant olm | 200 kg | 1-2 | cavern water | exotic pet, exotic mount, predator, amphibious | A giant amphibian predator found underground near water. |
| giant rat | 200 kg | 1-2 | cavern | exotic pet, exotic mount, steals food, steals drink | A gigantic rodent found underground. |
| gorlak | 50 kg | 1-3 | cavern | good biome, speaks | A small, round humanoid found wandering the caves deep underground.  Most of its body is taken up by a huge tusked mouth. |
| green devourer | 70 kg | 2-3 | cavern |  | A medium-sized monster walking on two clawed legs.  It has two mouths on the ends of a pair of tentacles.  It uses its mouths to digest its victims with acid and rows of razor-like teeth. |
| gremlin | 10 kg | 1-3 | cavern | exotic pet, speaks | A small humanoid creature with a mischievous, toothy grin. |
| helmet snake | 50 kg | 1-2 | cavern | exotic pet, predator | A snake-like creature living deep underground.  Its head is covered in armor so that is resembles the head of a dragon. |
| hungry head | 5.0 kg | 3-3 | cavern | predator, flies | A fearsome, long-toothed mouth the size of a man's head, flying on bat wings.  It is found in deep caves. |
| iron man | 70 kg | 3-3 | cavern | predator, not living | A man-shaped creature made of iron. |
| jabberer | 4,500 kg | 2-3 | cavern | exotic pet, trainable, exotic mount, predator | A huge monster that lurks in caverns deep under the earth.  It uses its wide beak to reach down and pluck up unsuspecting intruders. |
| large rat | 25 kg | 1-1 | cavern | exotic pet, steals food, steals drink | A huge rodent found underground. |
| magma crab | 30 kg | 3-5 | magma sea | fire immune | A small rock-eating creature that lives in molten rock.  It scurries on little feet and swims through liquid rock with sharp wings.  It uses magma to digest rock and spits out burning globs. |
| magma man | 70 kg | 3-4 | magma sea | predator, not living, fire immune | Magma moving in the shape of a man.  It has a cracked black crust. |
| manera | 60 kg | 2-2 | cavern | evil biome | A creature that crawls along the cavern ceiling with four long arms.  Its body is shaped as the head of a man with a mouth full of shark teeth.  It waits for its prey to pass below. |
| molemarian | 90 kg | 2-3 | cavern | predator | A hideous monster that has the body of a giant mole-rat and the torso of a mole-rat man.   It is found deep underground. |
| mud man | 70 kg | 3-3 | cavern water | predator, not living | A humanoid made of mud which lives near water underground. |
| naked mole dog | 60 kg | 1-1 | cavern | exotic pet, steals food, steals drink | A large, pale rodent with loose, hanging, hairless skin.  It has long teeth and an incredibly powerful bite.  It is found underground. |
| olm | 0.2 kg | 1-2 | cavern water | exotic pet, amphibious, vermin | A tiny amphibian, found in underground streams. |
| olm man | 0.2 kg |  | cavern water | amphibious | An animal person with the head of the amphibious olm.  It lives underground near water. |
| plump helmet man | 50 kg | 2-3 | cavern water |  | A small humanoid resembling a walking mushroom with arms and legs.  It lives far underground near water and soil. |
| pond grabber | 30 kg | 1-2 | cavern water | predator, aquatic | A small creature that lives in watery ditches deep underground.  It has a sharp beak and four tentacles with claws at the end. |
| purring maggot | 1.0 kg | 2-3 | cavern | exotic pet, vermin | A tiny larva which is prized for its milk. |
| reacher | 70 kg | 2-3 | cavern | evil biome | A humanoid monster found lurking far underground.  It feigns death, usually near water, until a victim passes by.  It then uses its long arms to drag its prey into the water to drown or strangle them. |
| reptile man | 50 kg | 1-3 | cavern water | predator, amphibious, speaks | These creatures are shaped like men covered with rough scales.  They have the head and tail of a lizard and possess a dark and evil intelligence.  They are found deep under the earth. |
| rodent man | 40 kg | 1-3 | cavern | predator, speaks | An evil humanoid with a long tail, black spiky fur and a twitching nose.  It lives far underground. |
| rutherer | 3,000 kg | 2-3 | cavern | exotic pet, exotic mount | A huge monster with an enormous tail, covered with thick fur.  They run on four legs and can be found deep under the earth. |
| serpent man | 50 kg | 1-3 | cavern water | predator, amphibious, speaks | A large white snake with the arms and torso of a man.  These creatures are evil and live far underground. |
| troglodyte | 60 kg | 1-2 | cavern | predator | A savage man-like cave creature. |
| troll | 250 kg | 1-3 | cavern | predator, evil biome | A huge humanoid monster with coarse fur, large tusks and horns. |
| voracious cave crawler | 900 kg | 2-3 | cavern | exotic pet, exotic mount, predator | A huge cave monster with hundreds of feet moving along the bottom of its long body.  In place of a head, it has an enormous toothy maw. |

## Wild surface animals

| Creature | Adult size | Habitat | Traits | Products |
|---|---|---|---|---|
| aardvark | 50 kg | grass/savanna, (tropical) | exotic pet |  |
| adder | 0.1 kg | wetland, forest, grass/savanna, (temperate) | exotic pet | eggs |
| albatross | 8.0 kg | ocean | exotic pet, flies | eggs |
| alligator | 400 kg | fresh/brackish water, wetland | exotic pet, exotic mount, predator, amphibious | eggs |
| alligator snapping turtle | 80 kg | fresh/brackish water, (temperate) | exotic pet, amphibious | eggs |
| anaconda | 100 kg | wetland, (tropical) | exotic pet, predator |  |
| angelshark | 15 kg | ocean | aquatic |  |
| armadillo | 7.5 kg | forest, grass/savanna, (tropical) | exotic pet |  |
| aye-aye | 2.5 kg | forest, (tropical) | exotic pet |  |
| badger | 15 kg | forest, grass/savanna, cold, (temperate) | exotic pet |  |
| barn owl | 0.5 kg | wetland, forest, grass/savanna, desert | exotic pet, flies | eggs |
| basking shark | 15,000 kg | ocean, (temperate) | aquatic |  |
| beaver | 20 kg | fresh/brackish water, (temperate) | exotic pet |  |
| bilou | 6.0 kg | forest, (tropical) | exotic pet |  |
| black bear | 120 kg | forest, cold, (temperate) | pet, predator, steals food, steals drink |  |
| black mamba | 5.0 kg | wetland, forest, grass/savanna, (tropical) | exotic pet | eggs |
| black-crested gibbon | 6.0 kg | forest, (tropical) | exotic pet |  |
| black-handed gibbon | 6.0 kg | forest, (tropical) | exotic pet |  |
| blacktip reef shark | 15 kg | ocean, (tropical) | predator, aquatic |  |
| blue shark | 300 kg | ocean | predator, aquatic |  |
| bluefin tuna | 600 kg | ocean | aquatic |  |
| bluefish | 15 kg | ocean | aquatic |  |
| bobcat | 8.0 kg | wetland, forest, desert, mountain | exotic pet, trainable |  |
| bonobo | 50 kg | forest, grass/savanna, (tropical) | exotic pet |  |
| bull shark | 150 kg | ocean, (tropical) | predator, aquatic |  |
| bushmaster | 8.5 kg | forest, (tropical) | exotic pet | eggs |
| buzzard | 1.4 kg | wetland, grass/savanna, desert, (temperate) | exotic pet, flies, steals food | eggs |
| capuchin | 3.5 kg | wetland, forest, (tropical) | exotic pet, steals items, steals food |  |
| capybara | 45 kg | wetland | pet, amphibious |  |
| carp | 40 kg | fresh/brackish water | aquatic |  |
| cassowary | 50 kg | forest, (tropical) | exotic pet | eggs |
| cheetah | 50 kg | grass/savanna, (tropical) | exotic pet, trainable, predator |  |
| chimpanzee | 50 kg | forest, grass/savanna, (tropical) | exotic pet |  |
| chinchilla | 0.5 kg | mountain | pet |  |
| coati | 6.0 kg | forest | exotic pet, steals items, steals food |  |
| cod | 50 kg | ocean, (temperate) | aquatic |  |
| coelacanth | 80 kg | ocean, (tropical) | aquatic |  |
| common skate | 100 kg | ocean | aquatic |  |
| common snapping turtle | 30 kg | fresh/brackish water, (temperate) | exotic pet, amphibious | eggs |
| conger eel | 50 kg | ocean, (temperate) | aquatic |  |
| copperhead snake | 0.5 kg | wetland, forest, (temperate) | exotic pet |  |
| cougar | 60 kg | forest, grass/savanna | exotic pet, predator |  |
| coyote | 15 kg | wetland, forest, grass/savanna, desert, cold, mountain, (temperate) | exotic pet |  |
| crab | 8.0 kg | ocean | amphibious |  |
| deer | 140 kg | forest, cold, (temperate) | exotic pet |  |
| desert tortoise | 5.5 kg | desert | exotic pet | eggs |
| dingo | 20 kg | — | exotic pet, predator |  |
| eagle | 4.0 kg | wetland, forest, grass/savanna, desert, cold, mountain | pet, flies | eggs |
| echidna | 10 kg | forest, grass/savanna, desert, (temperate) | exotic pet | eggs |
| elephant | 5,000 kg | forest, grass/savanna, (tropical) | pet, trainable, exotic mount |  |
| elephant seal | 3,000 kg | ocean | exotic pet, amphibious |  |
| elk | 300 kg | grass/savanna, cold, (temperate) | exotic pet |  |
| emperor penguin | 30 kg | ocean |  | eggs |
| emu | 35 kg | forest, grass/savanna, (temperate) | exotic pet | eggs |
| fox | 6.0 kg | forest, cold, (temperate) | exotic pet |  |
| frill shark | 60 kg | ocean | predator, aquatic |  |
| gazelle | 20 kg | grass/savanna, (tropical) | exotic pet |  |
| gila monster | 2.0 kg | desert | exotic pet, amphibious | eggs |
| giraffe | 1,000 kg | grass/savanna, (tropical) | exotic pet, exotic mount |  |
| gorilla | 150 kg | wetland, forest, (tropical) | exotic pet, trainable |  |
| grasshopper | 1 g | — |  |  |
| gray gibbon | 6.0 kg | forest, (tropical) | exotic pet |  |
| gray langur | 15 kg | forest, grass/savanna, desert | exotic pet, steals items, steals food |  |
| great barracuda | 60 kg | ocean, (tropical) | aquatic |  |
| great horned owl | 2.0 kg | wetland, forest, grass/savanna, desert, cold, mountain | exotic pet, flies | eggs |
| great white shark | 2,000 kg | ocean | predator, aquatic |  |
| grey parrot | 0.4 kg | forest, (tropical) | exotic pet, flies | eggs |
| grizzly bear | 200 kg | forest, cold, (temperate) | pet, trainable, exotic mount, predator, steals food, steals drink |  |
| groundhog | 3.0 kg | grass/savanna, (temperate) | exotic pet |  |
| halibut | 200 kg | ocean, (temperate) | aquatic |  |
| hammerhead shark | 500 kg | ocean | predator, aquatic |  |
| hare | 3.5 kg | grass/savanna, (temperate) | exotic pet |  |
| harp seal | 165 kg | ocean | exotic pet, amphibious |  |
| hippo | 1,500 kg | fresh/brackish water, (tropical) | exotic pet, amphibious |  |
| hoary marmot | 10 kg | mountain | exotic pet |  |
| honey badger | 14 kg | wetland, forest, grass/savanna, desert, (tropical) | exotic pet, steals food |  |
| hornbill | 2.5 kg | forest, (tropical) | exotic pet, flies | eggs |
| horseshoe crab | 2.0 kg | ocean | amphibious |  |
| hyena | 60 kg | grass/savanna, (tropical) | exotic pet, predator |  |
| ibex | 50 kg | grass/savanna, desert | pet |  |
| iguana | 4.0 kg | forest, (tropical) | exotic pet | eggs |
| impala | 50 kg | grass/savanna, (tropical) | exotic pet |  |
| jackal | 15 kg | grass/savanna, (tropical) | exotic pet |  |
| jaguar | 75 kg | desert, (tropical) | exotic pet, trainable, predator |  |
| kakapo | 3.0 kg | forest, grass/savanna, (temperate) | exotic pet | eggs |
| kangaroo | 90 kg | grass/savanna, desert, (temperate) | exotic pet | milk |
| kea | 1.0 kg | forest, grass/savanna, mountain, (temperate) | exotic pet, flies, steals items, steals food | eggs |
| kestrel | 0.2 kg | wetland, grass/savanna | pet, flies | eggs |
| king cobra | 6.0 kg | forest, (tropical) | exotic pet | eggs |
| kingsnake | 1.5 kg | forest, grass/savanna, desert, mountain, (temperate) | exotic pet | eggs |
| kiwi | 2.5 kg | forest, grass/savanna, (temperate) | exotic pet | eggs |
| koala | 10 kg | forest, (temperate) | exotic pet |  |
| leopard | 50 kg | desert, (tropical) | exotic pet, trainable, predator |  |
| leopard seal | 400 kg | ocean | exotic pet, amphibious |  |
| lion | 200 kg | grass/savanna, (tropical) | exotic pet, trainable, exotic mount, predator |  |
| little penguin | 1.5 kg | ocean |  | eggs |
| longfin mako shark | 80 kg | ocean | predator, aquatic |  |
| longnose gar | 20 kg | fresh/brackish water, (temperate) | aquatic |  |
| loon | 6.0 kg | fresh/brackish water, (temperate) | exotic pet, flies | eggs |
| lynx | 25 kg | forest, cold | exotic pet, trainable |  |
| mandrill | 20 kg | forest, (tropical) | exotic pet, trainable, steals items, steals food |  |
| manta ray | 2,300 kg | ocean, (tropical) | aquatic |  |
| marlin | 800 kg | ocean | aquatic |  |
| milkfish | 10 kg | ocean, fresh/brackish water, (tropical) | aquatic |  |
| mink | 0.8 kg | fresh/brackish water, (temperate) | exotic pet |  |
| mongoose | 3.0 kg | grass/savanna, (tropical) | exotic pet |  |
| monitor lizard | 100 kg | forest, grass/savanna, (tropical) | exotic pet | eggs |
| moose | 525 kg | forest, cold, (temperate) | exotic pet, exotic mount |  |
| mountain goat | 50 kg | mountain | pet |  |
| muskox | 285 kg | grass/savanna, cold, (temperate) | exotic pet, pack, wagon |  |
| narwhal | 1,200 kg | ocean | aquatic |  |
| nurse shark | 150 kg | ocean, (temperate) | aquatic |  |
| ocean sunfish | 1,000 kg | ocean | aquatic |  |
| ocelot | 25 kg | wetland, forest, grass/savanna, (tropical) | exotic pet, trainable |  |
| octopus | 5.0 kg | ocean | aquatic |  |
| one-humped camel | 500 kg | desert | pet, pack, mount | milk |
| opah | 200 kg | ocean | aquatic |  |
| opossum | 3.0 kg | forest, grass/savanna, (temperate) | exotic pet |  |
| orangutan | 80 kg | forest, (tropical) | exotic pet |  |
| orca | 5,000 kg | ocean | aquatic |  |
| osprey | 2.0 kg | ocean, fresh/brackish water, wetland | exotic pet, flies | eggs |
| ostrich | 90 kg | grass/savanna, desert, (tropical) | exotic pet | eggs |
| panda | 130 kg | forest, (temperate) | pet |  |
| pangolin | 5.0 kg | forest, grass/savanna, (tropical) | exotic pet |  |
| penguin | 4.0 kg | ocean |  | eggs |
| peregrine falcon | 0.6 kg | wetland, forest, grass/savanna, desert, cold, mountain | pet, flies | eggs |
| pike | 35 kg | fresh/brackish water, (temperate) | aquatic |  |
| pileated gibbon | 6.0 kg | forest, (tropical) | exotic pet |  |
| platypus | 2.0 kg | fresh/brackish water | pet, amphibious | eggs |
| polar bear | 400 kg | cold | exotic pet, trainable, exotic mount, predator, steals food, steals drink |  |
| porcupine | 9.0 kg | forest, grass/savanna, desert, cold, (temperate) | exotic pet |  |
| puffin | 0.8 kg | ocean | exotic pet, flies | eggs |
| python | 200 kg | forest, (tropical) | exotic pet | eggs |
| raccoon | 7.0 kg | forest, cold, (temperate) | exotic pet, steals items, steals food |  |
| rattlesnake | 7.0 kg | — | exotic pet |  |
| raven | 1.2 kg | wetland, forest, grass/savanna, desert, cold, (temperate) | pet, flies | eggs |
| red panda | 5.0 kg | forest, (temperate) | exotic pet |  |
| rhesus macaque | 5.0 kg | grass/savanna, (temperate) | exotic pet, steals items, steals food |  |
| rhinoceros | 3,000 kg | grass/savanna, (tropical) | exotic pet, war-trainable, exotic mount |  |
| river otter | 10 kg | fresh/brackish water | exotic pet, amphibious |  |
| saltwater crocodile | 800 kg | fresh/brackish water, wetland, (tropical) | exotic pet, exotic mount, predator, amphibious | eggs |
| sea lamprey | 20 kg | ocean, fresh/brackish water, (temperate) | predator, aquatic |  |
| sea otter | 30 kg | ocean, (temperate) | exotic pet, amphibious |  |
| shortfin mako shark | 80 kg | ocean | predator, aquatic |  |
| siamang | 12 kg | forest, (tropical) | exotic pet |  |
| silvery gibbon | 6.0 kg | forest, (tropical) | exotic pet |  |
| skunk | 4.0 kg | forest, grass/savanna, (temperate) | exotic pet |  |
| sloth | 6.0 kg | forest, (tropical) | exotic pet |  |
| sloth bear | 100 kg | forest, (tropical) | pet, steals food, steals drink |  |
| snowy owl | 2.0 kg | cold | exotic pet, flies | eggs |
| sperm whale | 25,000 kg | ocean | aquatic |  |
| spider monkey | 8.5 kg | forest, (tropical) | exotic pet |  |
| spiny dogfish | 30 kg | ocean | aquatic |  |
| sponge | 50 kg | ocean, fresh/brackish water | aquatic |  |
| spotted wobbegong | 80 kg | ocean | aquatic |  |
| stingray | 5.0 kg | ocean, fresh/brackish water, (tropical) | aquatic |  |
| stoat | 0.3 kg | forest, cold | exotic pet |  |
| sturgeon | 1,500 kg | ocean, fresh/brackish water, (temperate) | aquatic |  |
| swan | 10 kg | fresh/brackish water, wetland, (temperate) | exotic pet, flies | eggs |
| swordfish | 650 kg | ocean | aquatic |  |
| tapir | 200 kg | forest, (tropical) | exotic pet | milk |
| tiger | 225 kg | wetland, forest, grass/savanna, (tropical) | exotic pet, trainable, exotic mount, predator |  |
| tiger shark | 500 kg | ocean, (tropical) | predator, aquatic |  |
| tigerfish | 50 kg | fresh/brackish water, (tropical) | aquatic |  |
| two-humped camel | 500 kg | desert | pet, pack, mount | milk |
| vulture | 9.0 kg | grass/savanna, desert, (tropical) | exotic pet, flies, steals food | eggs |
| walrus | 1,500 kg | ocean | exotic pet, amphibious |  |
| warthog | 100 kg | grass/savanna, (tropical) | exotic pet |  |
| weasel | 0.2 kg | cold | exotic pet |  |
| whale shark | 20,000 kg | ocean, (tropical) | aquatic |  |
| white stork | 3.0 kg | wetland, grass/savanna | exotic pet, flies | eggs |
| white-browed gibbon | 6.0 kg | forest, (tropical) | exotic pet |  |
| white-handed gibbon | 6.0 kg | forest, (tropical) | exotic pet |  |
| whitetip reef shark | 15 kg | ocean, (tropical) | aquatic |  |
| wild boar | 80 kg | wetland, forest, grass/savanna | exotic pet |  |
| wolf | 40 kg | forest, grass/savanna, cold, (temperate) | exotic pet, predator |  |
| wolverine | 20 kg | forest, cold, mountain | exotic pet |  |
| wombat | 25 kg | forest, grass/savanna, mountain, (temperate) | exotic pet |  |

## Vermin

| Creature | Adult size | Habitat | Traits |
|---|---|---|---|
| anchovy | 0.2 kg | ocean, (temperate) | aquatic, fishable, vermin |
| anole | 90 g | forest, (tropical) | exotic pet, vermin |
| ant | 1 g | — | flies, vermin |
| axolotl | 0.2 kg | fresh/brackish water, (tropical) | exotic pet, amphibious, vermin |
| banded knifefish | 0.2 kg | fresh/brackish water, (tropical) | aquatic, fishable, vermin |
| bark scorpion | 3 g | forest, grass/savanna, desert, (tropical) | exotic pet, vermin |
| bat ray | 0.2 kg | ocean | aquatic, fishable, vermin |
| beetle | 1 g | — | vermin |
| black bullhead | 0.2 kg | fresh/brackish water, (temperate) | aquatic, fishable, vermin |
| blue jay | 0.1 kg | forest, grass/savanna, (temperate) | pet, flies, vermin |
| brook lamprey | 0.2 kg | ocean, fresh/brackish water, (temperate) | aquatic, fishable, vermin |
| brown bullhead | 0.2 kg | fresh/brackish water, (temperate) | aquatic, fishable, vermin |
| brown recluse spider | 1 g | forest, (temperate) | exotic pet, webs, vermin |
| bumblebee | 1 g | — | flies, vermin |
| bushtit | 5 g | forest, (temperate) | pet, flies, vermin |
| cardinal | 50 g | forest, grass/savanna, (temperate) | pet, flies, vermin |
| chameleon | 0.1 kg | forest, grass/savanna, desert, (tropical) | exotic pet, vermin |
| char | 0.2 kg | fresh/brackish water, (temperate) | aquatic, fishable, vermin |
| chipmunk | 0.3 kg | forest, (temperate) | exotic pet, vermin |
| clown loach | 0.2 kg | fresh/brackish water, (tropical) | aquatic, fishable, vermin |
| clownfish | 0.2 kg | ocean, (tropical) | aquatic, fishable, vermin |
| cockatiel | 90 g | grass/savanna, desert, (temperate) | pet, flies, vermin |
| crow | 0.5 kg | wetland, forest, grass/savanna, cold, (temperate) | pet, flies, vermin |
| cuttlefish | 1.0 kg | ocean | aquatic, fishable, vermin |
| damselfly | 1 g | fresh/brackish water | flies, vermin |
| dragonfly | 1 g | fresh/brackish water | flies, vermin |
| firefly | 1 g | — | flies, vermin |
| flounder | 0.2 kg | ocean, (temperate) | aquatic, fishable, vermin |
| fly | 1 g | fresh/brackish water | flies, vermin |
| flying squirrel | 0.2 kg | forest, (temperate) | exotic pet, vermin |
| glasseye | 0.2 kg | ocean, (tropical) | aquatic, fishable, vermin |
| grackle | 0.1 kg | grass/savanna, (temperate) | pet, flies, vermin |
| gray squirrel | 0.3 kg | forest, (temperate) | exotic pet, vermin |
| green tree frog | 0.1 kg | fresh/brackish water, wetland, (temperate) | exotic pet, amphibious, vermin |
| guppy | 0.2 kg | fresh/brackish water, (tropical) | aquatic, fishable, vermin |
| hagfish | 0.2 kg | ocean, (temperate) | aquatic, fishable, vermin |
| hake | 0.2 kg | ocean, (temperate) | aquatic, fishable, vermin |
| hamster | 0.1 kg | — | pet, gnaws, vermin |
| hedgehog | 0.8 kg | grass/savanna, (temperate) | pet, vermin |
| herring | 0.2 kg | ocean, (temperate) | aquatic, fishable, vermin |
| honey bee | 1 g | — | flies, vermin |
| jumping spider | 1 g | — | exotic pet, vermin |
| large roach | 1 g | — | pet, flies, vermin |
| leech | 0.1 kg | fresh/brackish water | exotic pet, vermin |
| leopard gecko | 50 g | desert | exotic pet, vermin |
| lion tamarin | 0.6 kg | forest, (tropical) | exotic pet, vermin |
| lizard | 0.2 kg | — | exotic pet, vermin |
| lorikeet | 0.2 kg | wetland, forest, (tropical) | pet, flies, vermin |
| louse | 1 g | — | vermin |
| lungfish | 0.2 kg | fresh/brackish water, (tropical) | amphibious, fishable, vermin |
| mackerel | 0.2 kg | ocean, (temperate) | aquatic, fishable, vermin |
| magpie | 0.2 kg | grass/savanna, (temperate) | pet, flies, vermin |
| mantis | 1 g | — | flies, vermin |
| masked lovebird | 90 g | forest, (tropical) | pet, flies, vermin |
| monarch butterfly | 1 g | — | flies, vermin |
| moon snail | 0.2 kg | ocean, (temperate) | aquatic, vermin |
| mosquito | 1 g | fresh/brackish water | flies, vermin |
| moth | 1 g | — | flies, vermin |
| mussel | 0.2 kg | ocean, fresh/brackish water | aquatic, fishable, vermin |
| nautilus | 0.5 kg | ocean | aquatic, fishable, vermin |
| oriole | 40 g | forest, (temperate) | pet, flies, vermin |
| oyster | 0.2 kg | ocean | aquatic, fishable, vermin |
| parakeet | 0.1 kg | forest, grass/savanna, (tropical) | pet, flies, vermin |
| peach-faced lovebird | 60 g | forest, grass/savanna, (temperate) | pet, flies, vermin |
| perch | 0.2 kg | fresh/brackish water, (temperate) | aquatic, fishable, vermin |
| pond turtle | 0.5 kg | fresh/brackish water | exotic pet, amphibious, fishable, vermin |
| rainbow trout | 0.2 kg | fresh/brackish water, (temperate) | aquatic, fishable, vermin |
| rat | 0.3 kg | — | pet, gnaws, vermin |
| red squirrel | 0.3 kg | forest, (temperate) | exotic pet, vermin |
| red-winged blackbird | 50 g | wetland, (temperate) | pet, flies, vermin |
| sailfin molly | 0.2 kg | fresh/brackish water, (temperate) | aquatic, fishable, vermin |
| salmon | 0.2 kg | ocean, fresh/brackish water, (temperate) | aquatic, fishable, vermin |
| sea nettle jellyfish | 0.2 kg | ocean, (temperate) | aquatic, fishable, vermin |
| seahorse | 0.2 kg | ocean | aquatic, fishable, vermin |
| shad | 0.2 kg | ocean, fresh/brackish water, (temperate) | aquatic, fishable, vermin |
| skink | 0.5 kg | desert | exotic pet, vermin |
| slug | 1 g | — | pet, vermin |
| snail | 1 g | — | pet, vermin |
| sole | 0.2 kg | ocean, (temperate) | aquatic, fishable, vermin |
| sparrow | 30 g | wetland, forest, grass/savanna, desert | pet, flies, vermin |
| spotted ratfish | 0.2 kg | ocean, (temperate) | aquatic, fishable, vermin |
| squid | 0.2 kg | ocean | aquatic, fishable, vermin |
| steelhead trout | 0.2 kg | ocean, fresh/brackish water, (temperate) | aquatic, fishable, vermin |
| termite | 1 g | — | vermin |
| thornback ray | 0.2 kg | ocean | aquatic, fishable, vermin |
| thrips | 1 g | — | flies, vermin |
| tick | 1 g | — | vermin |
| toad | 0.2 kg | fresh/brackish water | exotic pet, amphibious, vermin |
| white-spotted puffer | 0.2 kg | ocean, (tropical) | aquatic, fishable, vermin |
| worm | 0.1 kg | forest, cold | pet, vermin |
| wren | 40 g | wetland, forest, grass/savanna, desert | pet, flies, vermin |
| yellow bullhead | 0.2 kg | fresh/brackish water, (temperate) | aquatic, fishable, vermin |

## Giant animals

181 creatures: giant aardvark, giant adder, giant albatross, giant alligator, giant anaconda, giant anole, giant armadillo, giant axolotl, giant aye-aye, giant badger, giant bark scorpion, giant barn owl, giant beaver, giant beetle, giant black bear, giant black mamba, giant bluejay, giant bobcat, giant brown recluse spider, giant bushmaster, giant bushtit, giant buzzard, giant capuchin, giant capybara, giant cardinal, giant cassowary, giant chameleon, giant cheetah, giant chinchilla, giant chipmunk, giant coati, giant cockatiel, giant copperhead snake, giant cougar, giant coyote, giant crab, giant crow, giant cuttlefish, giant damselfly, giant deer, giant desert tortoise, giant dingo, giant dragonfly, giant eagle, giant echidna, giant elephant, giant elephant seal, giant elk, giant emu, giant firefly, giant fly, giant flying squirrel, giant fox, giant gazelle, giant gila monster, giant giraffe, giant grackle, giant grasshopper, giant gray langur, giant gray squirrel, giant great horned owl, giant green tree frog, giant grey parrot, giant grizzly bear, giant groundhog, giant grouper, giant hamster, giant hare, giant harp seal, giant hedgehog, giant hippo, giant hoary marmot, giant honey badger, giant hornbill, giant horseshoe crab, giant hyena, giant ibex, giant iguana, giant impala, giant jackal, giant jaguar, giant jumping spider, giant kakapo, giant kangaroo, giant kea, giant kestrel, giant king cobra, giant kingsnake, giant kiwi, giant koala, giant leech, giant leopard, giant leopard gecko, giant leopard seal, giant lion, giant lion tamarin, giant lizard, giant loon, giant lorikeet, giant louse, giant lynx, giant magpie, giant mandrill, giant mantis, giant masked lovebird, giant mink, giant monarch butterfly, giant mongoose, giant monitor lizard, giant moon snail, giant moose, giant mosquito, giant moth, giant mountain goat, giant muskox, giant narwhal, giant nautilus, giant ocelot, giant octopus, giant one-humped camel, giant opossum, giant orca, giant oriole, giant osprey, giant ostrich, giant otter, giant pangolin, giant parakeet, giant peach-faced lovebird, giant penguin, giant peregrine falcon, giant platypus, giant polar bear, giant pond turtle, giant porcupine, giant puffin, giant python, giant raccoon, giant rattlesnake, giant raven, giant red panda, giant red squirrel, giant red-winged blackbird, giant rhesus macaque, giant rhinoceros, giant roach, giant saltwater crocodile, giant skink, giant skunk, giant sloth, giant sloth bear, giant slug, giant snail, giant snapping turtle, giant snowy owl, giant sparrow, giant sperm whale, giant spider monkey, giant sponge, giant stoat, giant swan, giant tapir, giant thrips, giant tick, giant tiger, giant toad, giant tortoise, giant two-humped camel, giant vulture, giant walrus, giant warthog, giant weasel, giant white stork, giant wild boar, giant wolf, giant wolverine, giant wombat, giant wren, gigantic panda, gigantic squid, gigantic tortoise.

## Animal people

181 creatures: aardvark man, adder man, albatross man, alligator man, anaconda man, anole man, armadillo man, axolotl man, aye-aye man, badger man, bark scorpion man, barn owl man, beaver man, beetle man, black bear man, black mamba man, bluejay man, bobcat man, brown recluse spider man, bushmaster man, bushtit man, buzzard man, capuchin man, capybara man, cardinal man, cassowary man, chameleon man, cheetah man, chinchilla man, chipmunk man, coati man, cockatiel man, copperhead snake man, cougar man, coyote man, crab man, crow man, cuttlefish man, damselfly man, deer man, desert tortoise man, dingo man, dragonfly man, eagle man, echidna man, elephant man, elephant seal man, elk man, emu man, firefly man, fly man, flying squirrel man, fox man, gazelle man, giant tortoise man, gila monster man, giraffe man, grackle man, grasshopper man, gray langur man, gray squirrel man, great horned owl man, green tree frog man, grey parrot man, grizzly bear man, groundhog man, hamster man, hare man, harp seal man, hedgehog man, hippo man, hoary marmot man, honey badger man, hornbill man, horseshoe crab man, hyena man, ibex man, iguana man, impala man, jackal man, jaguar man, jumping spider man, kakapo man, kangaroo man, kea man, kestrel man, king cobra man, kingsnake man, kiwi man, koala man, leech man, leopard gecko man, leopard man, leopard seal man, lion man, lion tamarin man, lizard man, loon man, lorikeet man, louse man, lynx man, magpie man, mandrill man, mantis man, masked lovebird man, mink man, monarch butterfly man, mongoose man, monitor lizard man, moon snail man, moose man, mosquito man, moth man, mountain goat man, muskox man, narwhal man, nautilus man, ocelot man, octopus man, one-humped camel man, opossum man, orca man, oriole man, osprey man, ostrich man, otter man, panda man, pangolin man, parakeet man, peach-faced lovebird man, penguin man, peregrine falcon man, platypus man, polar bear man, pond turtle man, porcupine man, puffin man, python man, raccoon man, rat man, rattlesnake man, raven man, red panda man, red squirrel man, red-winged blackbird man, rhesus macaque man, rhinoceros man, roach man, saltwater crocodile man, skink man, skunk man, sloth bear man, sloth man, slug man, snail man, snapping turtle man, snowy owl man, sparrow man, sperm whale man, spider monkey man, sponge man, squid man, stoat man, swan man, tapir man, thrips man, tick man, tiger man, toad man, two-humped camel man, vulture man, walrus man, warthog man, weasel man, white stork man, wild boar man, wolf man, wolverine man, wombat man, worm man, wren man.

## Placeholders

4 creatures: centaur, chimera, griffon, wagon.
