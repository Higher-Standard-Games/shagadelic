# Work split

Three sessions work in this checkout at once. Each owns the areas below. Read [the design](SHAGADELIC.md) and [AGENTS.md](../../AGENTS.md) first.

| Owner | Area | Files it owns |
| --- | --- | --- |
| Session A (combat) | Fight movement, target memory, weapon behavior | `combat.rs`, `strafe.rs`, `aim.rs`, `weapons.rs`, `random.rs`, `sight.rs`, `tools/bake-washoi-sight.mjs` |
| Session B (traversal and lobby) | Jump, jump pad, and teleporter edges; lobby passing map and difficulty | `tools/bake-washoi-walk-graph.mjs`, a new traversal module, the Washoi `shagadelic-lobby` branch |
| Session C (game modes) | Server directives, objectives, S&D and team play | `orders.rs`, `tests/washoi_orders.rs`, objective planning in `navigation.rs` and `navigation/graph.rs` |

Shared files: `behavior.rs`, `mod.rs`, `difficulty.rs`, `navigation.rs`, `navigation/graph.rs`, `navigation/follower.rs`, `protocol/`, `control/`, and `SHAGADELIC.md`. Change them in small hunks, and say in the commit message what you touched there.

## Rules for working in one checkout

- Stage only your own hunks. Never `git add -A`, `git commit -a`, or bare `git stash`, and never revert or reformat changes you didn't make. Before committing, check `git diff --cached` for foreign lines. If `cargo fmt` reformats another session's uncommitted file, leave that diff for them.
- To commit part of a shared file, write the version you want and stage it with `git hash-object -w` and `git update-index --cacheinfo`. Verify the staged snapshot with `git checkout-index -a --prefix=<tmp>/` and a separate `CARGO_TARGET_DIR`.
- Run `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings` with and without `--features washoi`, and `cargo test` with and without it.
- Live tests share one local server: `node node_modules/vite-node/vite-node.mjs src/games/fps/cli/headless-netserver-v2.ts --port 7777 --map washoi_dm2` from `~/Desktop/wash/shag`. Bots from two sessions in the same match disturb each other's tests, so use another port when your test runs in parallel.
- `origin` is `github.com/Higher-Standard-Games/shagadelic`. Push only when the user asks.
- Update this file when ownership changes, and the design doc when a contract or milestone changes.

## Handover for session B: traversal and lobby wiring

### 1. Jump, jump pad, and teleporter edges

The TypeScript navigation cache for `washoi_dm2` (`~/Desktop/wash/shag/.cache/bots/maps/washoi_dm2.json`) has 360 walk, 22 fall, 63 jump, 4 teleporter, and 1 jump pad edges. `tools/bake-washoi-walk-graph.mjs` imports walk and fall edges, plus jumps with `requiredSpeed` 0. It drops the rest, which is why the Red Armor ledge, whose exit is a jump, and a few pockets stay unreachable. Spawn 3 is in such a pocket (see `the_cached_washoi_dm2_graph_plans_from_its_spawns`). A jump edge carries `requiredSpeed`, `expectedTicks`, `successMargin`, and `validated`.

The compatibility references are `src/games/fps/bot/sequences/TraverseJumppadSequence.ts`, `TraverseTeleporterSequence.ts`, `ValidatedJumpSequence.ts`, `TeleportExit.ts`, and `BotNavGraph.ts` in the SHAG checkout. Map triggers come from `extractTriggerVolumes` (`src/shared/bsp/TriggerTable.ts`), which the baker can reach through `HeadlessMapLoader.loadBspCollision`.

Suggested path:
1. Bake the extra edge kinds with what executing them needs. That is the takeoff speed and heading for a jump, the trigger volume for a pad, and the teleporter exit. Keep the 64 KiB initialization record limit in mind. The `washoi_dm2` graph is 12 KB today.
2. Let the planner use them. The area rule in `graph.rs` (`Areas`, Kosaraju) then joins the pockets to the main area. Recheck `walks_only_to_goals_the_bot_can_walk_back_from` and the spawn test.
3. Execute them as exact-movement sequences, as `learned.rs` `Traversal` does for recorded jumps. `BotPlayer::uses_exact_movement` and the virtual keys in `protocol/keys.rs` already step aside for exact movement. The aim controller resets after a script sets the view.
4. Verify live, as `washoi_bot_roams_between_graph_goals` does. A bot should reach the Red Armor and leave again.

### 2. Lobby passes map and difficulty

`ShagadelicRunnerPool.ts` on the Washoi `shagadelic-lobby` branch calls the supervisor without a map or difficulty. Lobby matches therefore get no navigation, sight, or learned routes, and always play medium. `tools/washoi-supervisor.mjs` already takes `start(matchId, endpoint, map)`, and `launch(matchId, { name, ticket, team, difficulty })` with a preset name. Per-map files come from `SHAGADELIC_NAVIGATION_DIR` (`<map>.json` and `<map>.sight`) and `SHAGADELIC_LEARNED_ROUTES_DIR`. The sister repos are on other branches. Ask the user before switching their checkout's branch.

## Session A plan (combat)

Done: strafing and hit dodges (`b83927c`), rocket and grenade sidesteps (`37eef1d`), chasing an enemy out of sight (`efe5de9`), and perception: a view cone, noticing delay, and hearing (`17db763`). On session A's own server (port 7791), two Insane bots reach a kill in nine of ten runs within 120 seconds.

Next:
1. Hunting: where to look for an enemy when none is known, after `states/BotRoam.ts` `hunt`.
2. Charged shots and weapon combos, after `updateChargedFire` and `BotWeaponCombos.ts`.
3. Cover peeking and high ground, after `BotCoverPeek.ts` and `findHighGround`.

## Notes between sessions

- `a7e0093` staged the whole design doc and took session A's uncommitted fight-movement paragraph with it. No harm, since the text was right, but stage doc hunks one by one.
