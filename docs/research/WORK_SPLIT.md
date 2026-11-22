# Work split

Three sessions work in this checkout at once. Each owns the areas below. Read [the design](SHAGADELIC.md) and [AGENTS.md](../../AGENTS.md) first.

| Owner | Area | Files it owns |
| --- | --- | --- |
| Session A (combat) | Fight movement, target memory, weapon behavior | `agents/player/combat/`, `agents/player/movement/`, `perception/`, `random.rs`, `tools/bake-washoi-sight.mjs` |
| Session B (traversal and lobby) | Jump, jump pad, and teleporter edges; lobby passing map and difficulty | `tools/bake-washoi-walk-graph.mjs`, a new traversal module, the Washoi `shagadelic-lobby` branch |
| Session C (game modes) | Server directives, objectives, S&D and team play | `agents/player/strategy/orders.rs`, `tests/washoi_orders.rs`, objective planning in `agents/player/navigation.rs` and `agents/player/navigation/graph.rs` |

Shared files: `agents/mod.rs`, `agents/player/mod.rs` (`BotPlayer`), `mod.rs`, `observation.rs`, `difficulty.rs`, `agents/player/strategy/goals.rs`, `agents/player/navigation*`, `hosting/net/` (session and protocol), `control/`, and `SHAGADELIC.md`. Paths are under `src/games/washoi/`. Change them in small hunks, and say in the commit message what you touched there.

## Rules for working in one checkout

- Stage only your own hunks. Never `git add -A`, `git commit -a`, or bare `git stash`, and never revert or reformat changes you didn't make. Before committing, check `git diff --cached` for foreign lines. If `cargo fmt` reformats another session's uncommitted file, leave that diff for them.
- To commit part of a shared file, write the version you want and stage it with `git hash-object -w` and `git update-index --cacheinfo`. Verify the staged snapshot with `git checkout-index -a --prefix=<tmp>/` and a separate `CARGO_TARGET_DIR`.
- Run `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings` with and without `--features washoi`, and `cargo test` with and without it.
- Live tests share one local server: `node node_modules/vite-node/vite-node.mjs src/games/fps/cli/headless-netserver-v2.ts --port 7777 --map washoi_dm2` from `~/Desktop/wash/shag`. Bots from two sessions in the same match disturb each other's tests, so use another port when your test runs in parallel.
- `origin` is `github.com/Higher-Standard-Games/shagadelic`. Push only when the user asks.
- Update this file when ownership changes, and the design doc when a contract or milestone changes.

## Done: Washoi brain separation

The separation landed through `bfd9308`. Session A continues with gameplay (hunting next), and session B with the traversal and lobby handover below. The assignments that follow are kept for reference.

The gameplay ownership above describes the parity work. For the architecture milestone, use the assignments below. The baseline includes the perception changes in `17db763`. Capture their behavior before changing the brain or switching the live scheduler.

| Session | First deliverable | File ownership during the refactor |
| --- | --- | --- |
| A | Define Washoi-owned observations and actions with B. Port the production brain to those types and caller-supplied simulation time. Preserve perception, chase memory, skill priority, and exact maneuvers. | Own the new game-domain types, `behavior.rs`, its gameplay helpers, and the Washoi module exports. |
| B | Map reconstructed snapshots and ordered events into A's observation contract and actions into paced wire inputs. Move decision execution under the scheduler's decision budget. | Own `session.rs`, `protocol/`, and any necessary host or runtime changes. Keep socket admission, ACKs, backpressure, removal, and lobby shutdown working. |
| C, if available | Capture a short reference trace on the pre-refactor baseline. Include elapsed times, a transient event and an exact input sequence. Then replay the same fixture through the synchronous brain and compare outputs with the live path. | Own replay fixtures and independent integration tests. If a test needs a private hook, ask the owning session to expose it. B owns this verification if C is unavailable. |

Session B also took over the hosted scheduler and replay work from `763c135` (`session.rs`, `protocol/connection.rs`, `protocol/keys.rs`, `runtime/mod.rs`, `control/washoi.rs`, `tests/washoi_hosted_replay.rs`), whose author is no longer active. Session A works in its own worktree and lands brain commits on `main`. Snapshots map through `protocol::observe` (`5e901d7`), and `Controls` encode through the movement keys (`b22a029`).

A and B first agree on observation, ordered event, action, and time semantics against the trace. B can exercise burst delivery with a zero decision budget while A ports gameplay. Wire the production paths together after the replay matches. Finish with a private lobby smoke covering admission, movement, traversal, combat, and shutdown. Keep HUNCH training and waypoint schema changes for their own milestones.

## Handover for session B: traversal and lobby wiring

### 1. Jump, jump pad, and teleporter edges

The TypeScript navigation cache for `washoi_dm2` (`~/Desktop/wash/shag/.cache/bots/maps/washoi_dm2.json`) has 360 walk, 22 fall, 63 jump, 4 teleporter, and 1 jump pad edges. `tools/bake-washoi-walk-graph.mjs` imports walk and fall edges, plus jumps with `requiredSpeed` 0. It drops the rest, which is why the Red Armor ledge, whose exit is a jump, and a few pockets stay unreachable. Spawn 3 is in such a pocket (see `the_cached_washoi_dm2_graph_plans_from_its_spawns`). A jump edge carries `requiredSpeed`, `expectedTicks`, `successMargin`, and `validated`.

The compatibility references are `src/games/fps/bot/sequences/TraverseJumppadSequence.ts`, `TraverseTeleporterSequence.ts`, `ValidatedJumpSequence.ts`, `TeleportExit.ts`, and `BotNavGraph.ts` in the SHAG checkout. Map triggers come from `extractTriggerVolumes` (`src/shared/bsp/TriggerTable.ts`), which the baker can reach through `HeadlessMapLoader.loadBspCollision`.

Suggested path:
1. Bake the extra edge kinds with what executing them needs. That is the takeoff speed and heading for a jump, the trigger volume for a pad, and the teleporter exit. Keep the 64 KiB initialization record limit in mind. The `washoi_dm2` graph is 12 KB today.
2. Let the planner use them. The area rule in `graph.rs` (`Areas`, Kosaraju) then joins the pockets to the main area. Recheck `walks_only_to_goals_the_bot_can_walk_back_from` and the spawn test.
3. Execute them as exact-movement sequences, as `navigation/learned.rs` `Traversal` does for recorded jumps. `BotPlayer::uses_exact_movement` and the virtual keys in `protocol/keys.rs` already step aside for exact movement. The aim controller resets after a script sets the view.
4. Verify live, as `washoi_bot_roams_between_graph_goals` does. A bot should reach the Red Armor and leave again.

### 2. Lobby passes map and difficulty

`ShagadelicRunnerPool.ts` on the Washoi `shagadelic-lobby` branch calls the supervisor without a map or difficulty. Lobby matches therefore get no navigation, sight, or learned routes, and always play medium. `tools/washoi-supervisor.mjs` already takes `start(matchId, endpoint, map)`, and `launch(matchId, { name, ticket, team, difficulty })` with a preset name. Per-map files come from `SHAGADELIC_NAVIGATION_DIR` (`<map>.json` and `<map>.sight`) and `SHAGADELIC_LEARNED_ROUTES_DIR`. The sister repos are on other branches. Ask the user before switching their checkout's branch.

## Session A plan (combat)

Done: strafing and hit dodges (`b83927c`), rocket and grenade sidesteps (`37eef1d`), chasing an enemy out of sight (`efe5de9`), perception (`17db763`), the brain's side of the separation (`7ead674` to `cefd2cf`), weapon combos (`e8b1990`), cover peeking (`565fb7a`), high ground (`a189c5f`), charged blade blasts (`b604ac2`), the enemy estimate with the finishing switch (`7b5bd29`), order priority over fight movement (`1460384`), readying a gun for heard enemies, holding still to plant or defuse, and keeping to the upper ground (pending S&D checks). Hunting turned out to be mostly covered by chase and roaming, and the design records why. The rest needs the strategy layer's weight profiles.

## Parity backlog

The goal is every TypeScript bot skill in `src/games/fps/bot/`, not a sample. `tools/bench-washoi-ab.mjs` orders the work: take next what costs the Rust bot most against the TypeScript bot. Line counts are the TypeScript sources.

Session A, decisions and combat:
- [x] Mood and mistakes: `BotMood.ts` (160). Wild shots, wrong weapon, item glances, forgetting an enemy.
- [x] Fire safety: `BotFireSafety.ts` (97).
- [x] Goal arbiter and considerations: `goals/` (about 1,500). Covers `GoalArbiter`, `ItemGoalPlanner`, `ItemGoalSource`, and `DirectiveGoalSource`, plus the Danger, ItemDenial, ContestedItem, ItemValue, RouteCost, TeamPlay, and ObjectivePush considerations.
- [x] Strategy layer: `strategy/Strategist.ts` (448), `strategy/OpponentModel.ts` (59), the rest of `strategy/MatchRead.ts`, `BotTerritory.ts` (62), and `states/BotRoam.ts` roam choice (`goals/RoamChoice.ts`).
- [x] Learned strategy priors from earlier matches: `intel/` (393), in `strategy/intel.rs`. The supervisor's `intelDir` (`SHAGADELIC_INTEL_DIR`, SHAG's `config/bot-intel`) passes `people.json` and `<map>.json`. Building the store and exporting baselines stays with the TypeScript tools. The retreat's escape out of the enemy's sight is in `movement/escape.rs`.
- [x] Look behavior: `view/BotLook.ts` (226).
- [x] Blade lunge: `sequences/SlideBladeSequence.ts` (173).
- [x] Decision traces: `DecisionTrace.ts` (53) and replayable decision records.
- [x] Enemy major pickups credited in the estimate: `BotPerception` pickup hearing by item.

Session B, movement, navigation, and team play:
- [ ] Movement sequences: `RouteCrouchslideSequence` (158), `RouteStrafeJumpSequence` (384), `ItemSlideApproachSequence` (152), `LedgeSlideJump` (107), and `FlyingStart` (175). Route and item slides (`navigation/slide.rs`) and straight-lane hop chains (`navigation/hop.rs`) are ported. Hop chains bent along the route are on branch `hopline-b` and parked. They doubled airtime to TypeScript's level without raising speed, in 32 traced Duels with a held chord and the view sweeping at the top turn speed. One hypothesis to test first: at the speeds a chain starts from, Washoi's air acceleration (`airAcceleration` 1, `maxAirSpeed` 30) gains too little per hop, and TypeScript's high speeds come from longer stretches or jump pads. `LedgeSlideJump` is ported (`navigation/ledge_slide.rs`) and proven by rollout. It rarely fires live, because the bot reaches dm2's short approaches already slow, which is the run-speed gap, not the jump (B's hold experiment, recorded in SHAGADELIC.md). `FlyingStart` is ported (`navigation/flying_start.rs`, `47623cc`) over the map's own trigger table. All five sequences are now ported. What remains here is speed, which is tuning after the parity milestone.
- [x] `LiquidEscape.ts` (56) and `TeleportExit.ts`. TeleportExit is ported (`e632cb9`). LiquidEscape is ported, with sight format v2 carrying slime and lava. Washoi's server does hurt a body in lava (checked 2026-11-22). A fixture spawned an idle bot on the floor of `washoi_dm3`'s lava pool, brush 108, at (-1064, -7, -824), and the server's `pointContents` there reported lava, shared contents 4. Its health went 100, 70, 40, 10 over 2.1 s, 30 a second, with the body's `liquidContents` at 4 and water level 1. Where lava overlaps solid brushes, `pointContents` reports both. `washoi_dm2` has no slime or lava brushes, so the escape never fires there. The fixture is session B's scratch `lava-fixture.mjs`; rebake dm3's sight as v2 for the bot to see it.
- [ ] Local steering and unsticking: `BotVisionTracer.ts` (534), `movement/BotSteering.ts` (363), and `BotBlockedMove.ts`. `BotBlockedMove` and the brain's `_turnFromBlock` are in `navigation/unstick.rs`. `BotSteering` and the tracer are ported on branch `steering-a` (session A): ground clearance (`navigation/clearance.rs`), the air turn and unplanned pad rides, and rollout-proven climbs and wall jumps (`navigation/tricks.rs`), over the BSP trigger table. They land if the bench against main is no worse. The first bench, ground clearance only with graph walks off the line, was neutral to slightly worse: the frag gap was -0.75 against -0.44, inside the noise, with non-TS deaths at 5 against 8. It turned the bot on 20% of route decisions, so route walks now steer as on a line.
- [x] Team tactics, callouts, and chat: `BotCallouts.ts` (168), `chat/` (304), and the TeamPlay consideration. Done by session A in `agents/player/callouts.rs` and `chat.rs`. Locations are baked with `tools/bake-washoi-locations.mjs` into `<map>.locations.json` beside the navigation file.
- [ ] Game-mode parity: S&D rounds, Duel, FFA, and Amphi. Every directive kind a Washoi gametype issues is handled, and guards patrol and leash (`9507ab5`). Live S&D plants passed 5 of 6 at `854edcf`, 1 of 4 at `723ea62`, and 5 of 8 at `1301e4a` after the lane-point, lane-join, two-way-link, and ledge fixes (`bafbab5` to `1301e4a`). The remaining failures are carriers dying, then the next round running past the 80-second test window. **Session B is on the carrier deaths** (claimed 2026-11-22): why carriers die before planting where TypeScript carriers don't, measured from decision traces. Who defuses is the server's choice. Duel, FFA, and Amphi issue no orders, so their parity is the goal arbiter's weights.

Mark items here as they land, with the commit.

## Notes between sessions

- The agents/hosting split moved the player into `agents/player/` and the WebSocket session and protocol into `hosting/net/`.
- The module regrouping moved files into `combat/`, `movement/`, `perception/`, `strategy/`, and `navigation/learned.rs`. Commit hashes in this file and the design refer to the old flat paths.

- `a7e0093` staged the whole design doc and took session A's uncommitted fight-movement paragraph with it. No harm, since the text was right, but stage doc hunks one by one.

## HUNCH

The HUNCH session reports here. `docs/research/HUNCH.md`, "Next experiment and handoff", assigns the work.

Session C (Codex) took over HUNCH on 2026-11-22: the label fixes (items 1 and 2, H0 and H2), the 16-bot shadow measurement (item 5), and training and evaluation on the human recordings. Session B's movement work stays under Movement sequences above.

- [x] HUNCH API v1 library (session A), after HUNCH.md "Model architecture contract" and "RPG and hockey fixtures for the public API". `src/hunch/` holds the following:
  - `Spec`: decision ID, schema, and feature names.
  - Artifact format 2, which names its decision. Format 1 still loads as its schema's decision, `washoi.goal.v0` as `washoi.goal`.
  - `Model::parse` from memory, and `Model::load` from a file, return `Loaded` with the artifact's SHA-256 `Digest`.
  - `Mode` (`off`, `shadow`, `on`), which Washoi's `HunchMode` re-exports.
  - ID-labelled `Episode`s, with `Label::Chose` and `Label::Preferred` (a preference trains on its two candidates alone), and `Source` (`human`, `designer`, or `rule`), trained by `train(spec, episodes, training)`. The index-labelled `fit` stays underneath.
  - `Bindings`, which resolve an archetype's binding, then the decision's default, then the rule. They refuse duplicates, a mode with no model, and another decision's model, and share identical artifacts by digest.
  - `tests/hunch_api.rs` covers the healer preference and the hockey choice, trained, loaded from bytes, and replayed back to typed actions by ID. With equal widths, a swapped artifact fails on its decision. Carrier and winger give the same result in either order, and on another thread. Bindings with an override, an explicit off, shadow executing the rule, and a second decision.
- [ ] Session C: move `src/bin/hunch.rs` to the v1 training interface. `extract` writes `Episode` lines (`decision`, `schema`, `group` as the match or session, `source`, `ids`, `rows`, `label`), and `train` calls `hunch::train` instead of `fit`. Freeze v1 once extraction writes episodes.
- [x] Washoi bindings (session A):
  - The lobby init takes `hunch_manifest`, a JSON list of `{ archetype?, decision, mode?, artifact? }` read by `Bindings::from_manifest`, with artifacts relative to the manifest. The supervisor passes it from `SHAGADELIC_HUNCH_MANIFEST`.
  - `hunch_file` and `hunch_mode` remain shorthand for one default binding of `washoi.goal`. Both forms together, or a mode beside a manifest, are refused.
  - Each admitted bot resolves archetype `player` (`BotPlayer::HUNCH_ARCHETYPE`) for `washoi.goal`.
  - `DecisionTrace.hunch_model` names the bound model's digest.

Handoff from B to C: B drafted items 1 and 2 before the move. Neither is built or tested, and C may use them or not. Both are in `C:/tmp/hunch-handoff-b/`:
- `hunch-labels.patch`, against `src/bin/hunch.rs` at `6e439f4`, adds a `Labels` struct:
  - An `ItemPickup` naming the seat labels the record first.
  - An item that vanishes from the mask labels one frame later, unless an event names another taker that frame or the next.
  - The 10-second horizon holds the record before it is assigned.
  - The seat's `Death`, or `MatchState` phase 3 or 4, closes pending records.
  - New `Record` fields, all with serde defaults: `evidence`, `ambiguous` (another player within take distance), `pickup_tick`, `life`, and `closed` (`labelled`, `horizon`, `death`, `round_end`, or `end`).
  - Four unit tests.
- `hunch-cohort.mjs` runs with vite-node from a SHAG checkout:
  - It dedupes `.shagdemo`, gz, and zip files by decompressed SHA-256 and keeps every path.
  - Each seat gets human `confirmed`, `automated`, or `unknown`. Only a sessions file confirms a seat. The chosen name, default name, and account are kept as evidence.
  - It splits 70/15/15 by session, or by match when there is no session, with a seed. `--freeze` keeps earlier splits.

- [x] Item 3 (session A), what the model could see. `hunch_rows_ignore_an_opponent_the_bot_cannot_perceive` gives three bots identical worlds except one opponent. Moved between two spots behind a wall, unseen and unheard, it leaves the goal candidates and every feature row byte-identical. Put on the bot's run in view, it changes them. Models now load only when their feature names match `hunch_features::NAMES` in order (`Model::validate_features`), in `BotPlayer::with_hunch` and `hunch evaluate`.
- [x] Item 5 (session A), shadow wiring. The lobby init takes `hunch_file` and `hunch_mode` (`shadow`, the default, or `on`). The supervisor passes them from `SHAGADELIC_HUNCH_FILE` and `SHAGADELIC_HUNCH_MODE`. The model is loaded and validated once, against the schema, the dimensions, and the feature names, and every admitted bot shares it. The init refuses a model for other features, an unknown mode, or a mode with no model (`a_hunch_model_must_match_the_goal_schema`). `a_hunch_model_in_shadow_leaves_the_controls_unchanged` replays identical observations and seeds with no model and with two opposite models in shadow, and requires identical controls and traced goals. Deciding, one of the same models changes the goal, so shadow really consults the model. Fixed along the way: in `on` mode, `DecisionTrace.goal` named the arbiter's goal, not the model's goal the bot pursued. The measuring half of item 5 moved to session C and is still open: the 16-bot latency, input gaps, disagreements, and clips.
