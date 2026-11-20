Progression is focused on story, interactions and quests. To progress through the game, you participate in role playing as a character in the world. The gameplay challenge comes from completing these quests and interactions, the player is rewarded by moving the story forward, moving it to completion and being recorded in the worlds history. There exists a main world quest line which the player plays through.

Each quest is conceptually a miniature puzzle which requires exploration and an understanding of game mechanics. The overall player gameplay flow is as follows:

- Explore the world. As you explore, you gain items and materials, encouraging you to settle somewhere in the world so you can protect your things and keep them in one place.
	- You level up skill and are encouraged to try things out, possibly disguised as simple quests.
- Discover a quest. This can be a side quest related to a location or be relevant to the main quest.
- Prepare yourself for the quest requirements. This is potentially a bulk of the gameplay. If the quest is to bring an item someplace, then doing required actions to get the item, craft it or deliver it are the gameplay. Players can prepare by adjusting their skills and items to best suit the task.
- Complete quest. Once a quest is complete, the quest area / conditions potentially reset to allow other players to play it too. This depends on the quest type.

### Adventure And Editor Mode
Adventure mode is the default, players travel the world and build homes. They complete quests. If they choose to settle, they can begin optionally expanding their home, turning it into a site. They manage the site as a mayor.

The current fortress mode will still conceptually exists, however it will be done from adventure mode as a player, rather than a disembodied camera. There is no such thing as embark, people join your settlement over time or are born within it.

Editor mode is a creative mode which lets you access and edit code in script blocks. It is like minecraft creative mode and its purpose is to test generation, prefabs, quests aswell as to create all the aforementioned things.
### World Design
The need to find quests, prepare and complete them in an MMO setting requires a vast number of quests and things to do. Gameplay could be drawn out by making preparations harder, like making skill tree mastery gain slower if there are a lot of players.

The world is dived into acts which are time gated in a multiplayer setting, in single player it would be quest gated. All players goal is to complete quests in the world for their own advancement and the advancement of the world quest.

The world quest line is optionally on a loop, so once it completes there might be a grace period after which the quest line restarts, possibly harder than before.

The world coordinator can spawn events and quests. These can be player defined and are defined using the games scripting language. This essentially creates something like a quest / town / event template whereby the code defines how to generate a building, a town, its people, etc. To get a quest to appear, the code chooses a person, place and so on to create the setup for the quest and inserts it into the world.

- Quest creator
- Site generator

Sites are generated during world gen, quests are generated once world gen finishes. Quests are created by the world coordinator based on where players are currently playing live. There is no point spawning quests in places that players aren't in.
### Quest Script

Package managed and enabled during world gen. A package can add:
- Dialogue trees for quests in standard format
- Prefabs to use during worldgen
- Code to direct worldgen
- Add new quests that are considered to be added after worldgen
- Create / edit world quest line / acts
- Prefabs need to be able to contain entities and script blocks. During world gen, prefabs are placed to create sites. Quest blocks are placed into quest-relevant regions, the quest coordinator then, in order to setup the quest, according to the quest uploads it to one or more regions.
	- Script blocks are a way to access a regions raw scripts / quests. They do not inherently contain the scripts, they are an access point which can be access in adventure / editor mode.
	- The world coordinator reads all its packages and stores quest kickoff requirements in db.
	- Periodically the coordinator checks all quest start requirements to see if any need to be created.
	- If so, jobs are created in db to create the quests.
	- E.g In creating a quest, the coordinator chooses to create a quest for finding a lost animal. The quest is read from the package, is creation requirements have already been checked to have been met. The quest script code file will be loaded into the region's script block. It will be imported into the world potentially with input from the coordinator wrt its spawn requirements, the things that made it meet them. The coordinator is hands off at this point.
	- This script blocks execution in the region is what will setup the quest and manage it. Once the quest is complete, the quest removes itself from the script block.

Package management is modelled after nix except for the functional lazy loading. A package declares its dependencies declarative , they get fetched and managed via semantic versioning and hashing.

Some quests might require a special structure to be created during worldgen for the purpose of the quest, without that package being enabled during worldgen, the structure will not exist and the quest will never fulfil its requirements to be instantiated.

#### Package Server

**Future work**: Packages need to be uploaded to a package server so that they can be search, browesed, downloaded and enabled for a world during worldgen or after worldgen.  For now packages should be stored locally and bundled with the app.
