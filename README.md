# SHAGADELIC

Game AI as a service, in Rust. SHAGADELIC runs the AI players for one game
session. A host supervises it as a child process, or a game embeds its library
and drives it from the simulation loop. [Washoi](https://github.com/Higher-Standard-Games/wash-fps)
is the first integration: each Washoi bot joins as a headless client and plays
through the same input protocol as a person.

It is a sister of [SHAG](https://github.com/Higher-Standard-Games/shag), the
TypeScript engine whose bots set the behavior baseline, and of
[SHAGGER](https://github.com/Higher-Standard-Games/shagger) and
[wash-fps-rust](https://github.com/Higher-Standard-Games/wash-fps-rust), whose
shared crates give it Washoi's own movement, collision, and wire types.

The design, its findings and its milestones are in
[docs/research/SHAGADELIC.md](docs/research/SHAGADELIC.md). HUNCH, the learned
decision policy, has its plan in [docs/research/HUNCH.md](docs/research/HUNCH.md),
and [docs/research/WORK_SPLIT.md](docs/research/WORK_SPLIT.md) tracks who is on what.

## Features

The goal is parity with the TypeScript bots, then better decisions within a
predictable CPU budget. Status is opt-in and playable: the bundled Washoi lobby
admits Rust bots into private matches. Rated play waits on rating and match
report parity.

- **An embeddable runtime.** One scheduler per lobby, driven by the caller's
  simulation clock with event, input, and decision budgets. It never sleeps or
  starts threads of its own, so pausing freezes AI deadlines and replays step a
  tick at a time.
- **Lobby hosting.** A versioned JSON-lines control channel for readiness,
  spawns, removals, and shutdown, with the lobby keeping its authority over
  seats and admission. The Node supervisor `tools/washoi-supervisor.mjs` starts
  one child per match.
- **Fair perception.** Bots decide only from what they could perceive: a view
  cone, sound ranges, and line of sight traced through the map's baked static
  collision. Remembered item timings are blurred by skill.
- **Combat.** A port of the TypeScript aim controller, with reaction time,
  tracking, and prediction by difficulty. Weapon choice by range, strafing, hit
  dodges, rocket sidesteps, cover peeks, high ground, chases, blade lunges, and
  moods and mistakes that make skill levels feel human.
- **Navigation.** Baked walk graphs from SHAG's nav cache, with attachment from
  anywhere on a walk, stall recovery, jump pads, teleporters, cache jumps, and
  recorded trickjumps (learned routes). It escapes slime and lava, and unsticks
  from walls.
- **Movement through Washoi's own physics.** Sprint and slide rules as the game
  applies them, crouch-slides, strafe-jump chains with human-possible keys, and
  rollouts of Washoi's `MovementProfile` against the map's real BSP collision.
  Rollouts prove a move before making it: a ledge slide jump, or joining a
  learned route's script on the run (a flying start).
- **Game modes and team play.** Server directives for walking, holding,
  guarding, escorting, planting, and defusing in Search & Destroy. Goal
  arbitration across pickups, orders, hunts, and ground. Team callouts by
  location name, and chat lines with personalities.
- **HUNCH.** A small learned policy that scores the same candidates the goal
  arbiter weighs, trained offline from human demos, and run in shadow or live
  beside the rule. See [HUNCH](#hunch) below.
- **Traces.** Per-decision JSON traces (`SHAGADELIC_TRACE_DIR`) record what
  each bot saw, considered, chose, and which mover steered it, for replay,
  benchmarks, and review.

Full Duel, FFA, Amphi, and S&D round parity remains open. The feature matrix in
the design document has the evidence and the open work for each area.

## Building and checks

Washoi support is the `washoi` feature. It pulls in `shagger_shared` and
`washoi_movement` by pinned git revision, and both share one `shagger_shared`.

```sh
cargo build --release --features washoi
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --features washoi -- -D warnings
cargo test
cargo test --features washoi
```

The binaries are `shagadelic`, the lobby child, and `hunch`, HUNCH's offline
tools. Live tests against a local Washoi server are `#[ignore]`d. The Node
tests take the built executable:

```sh
node tests/control.test.mjs target/debug/shagadelic.exe
node tests/washoi-supervisor.test.mjs target/debug/shagadelic.exe
node tests/washoi-snd.test.mjs <shagadelic.exe> <SHAG checkout> <navigation dir>
```

On machines short of memory, build with `-j 1`.

## Map assets

A bot needs baked files for its map. Each baker runs under Washoi's vite-node
with a SHAG checkout as the working directory, and the child checks that every
file names the same map:

| File | Baker | Gives the bot |
| --- | --- | --- |
| `<map>.json` | `bake-washoi-walk-graph.mjs` | The walk graph, maneuvers, pickup goals, and learned-route links (`--learned`). |
| `<map>.sight` | `bake-washoi-sight.mjs` | Static line-of-sight geometry, and slime and lava volumes. |
| `<map>.collision` | `bake-washoi-collision.mjs` | The map's collision BSP, trimmed to the lumps collision reads, for movement rollouts. |
| `<map>.locations.json` | `bake-washoi-locations.mjs` | Location names for team callouts. |

```sh
node node_modules/vite-node/vite-node.mjs <SHAGADELIC>/tools/bake-washoi-walk-graph.mjs -- --shag-cache washoi_dm2 out/washoi_dm2.json
node node_modules/vite-node/vite-node.mjs <SHAGADELIC>/tools/bake-washoi-sight.mjs -- washoi_dm2 out/washoi_dm2.sight
node node_modules/vite-node/vite-node.mjs <SHAGADELIC>/tools/bake-washoi-collision.mjs -- washoi_dm2 out/washoi_dm2.collision --expect-sha <bsp_sha256>
```

A collision file carries the full BSP's SHA-256, and the child refuses one whose
hash differs from its walk graph's. The supervisor reads `<map>.*` from
`SHAGADELIC_NAVIGATION_DIR`, learned routes from `SHAGADELIC_LEARNED_ROUTES_DIR`,
and SHAG's bot intel from `SHAGADELIC_INTEL_DIR`.

## The v1 control protocol

The host talks to the child over stdin and stdout, one JSON record per line,
each at most 64 KiB. stderr is for diagnostics. The first record initializes
the lobby:

```json
{"type":"initialize","version":1,"lobby_id":"lobby-42",
 "washoi_endpoint":"ws://127.0.0.1:7777","washoi_map":"washoi_dm2",
 "navigation_file":"nav/washoi_dm2.json","sight_file":"nav/washoi_dm2.sight",
 "collision_file":"nav/washoi_dm2.collision","learned_routes_file":"routes/washoi_dm2.json",
 "locations_file":"nav/washoi_dm2.locations.json",
 "people_file":"intel/people.json","baseline_file":"intel/washoi_dm2.json",
 "hunch_file":"models/goal.json","hunch_mode":"shadow"}
```

Every field after `lobby_id` is optional. The child validates the assets,
then answers `{"type":"ready","version":1,"lobby_id":"lobby-42"}`, or exits with
an error. After that:

| Host sends | Child answers |
| --- | --- |
| `{"type":"spawn","id":"b1","name":"Rex","ticket":"…","team":"red","difficulty":"hard"}` | `bot_status` (`pending`), then `bot_admitted` with the server's `player_id`, or `bot_ended` with a reason. |
| `{"type":"remove","id":"b1","generation":3}` | `bot_ended`. A pending admission is cancelled. |
| `{"type":"shutdown"}` | The child closes every bot and exits. |

Difficulty presets are `easy`, `medium`, `hard`, and `insane`. A bot is keyed
by its host ID plus a spawn generation, so a late result from a replaced bot is
never mistaken for the new one. An unusable request gets `bot_request_failed`.
The child exits when its owner's channel closes, and the host shuts it down
when the game server ends. Credentials such as seat tickets travel only in records, never
on command lines or in logs.

## The library

Embedding hosts use the runtime directly:

```rust
use shagadelic::runtime::{Budget, GameSession, Runtime};

let mut runtime = Runtime::new(my_session); // impl GameSession
runtime.spawn("bot-1".into(), spawn_options)?;
loop {
    // Simulation time, not wall-clock time: pause and replay are exact.
    runtime.advance(sim_time, Budget { events: 128, inputs: 64, decisions: 64 })?;
    // ...apply the session's actions, step the world...
}
```

A `GameSession` begins and removes entities, reports events (`Admitted`,
`Ended`, `WakeDecision`, `SessionEnded`), and runs each entity's input or
decision update, returning its next deadline. The Washoi integration provides
`WashoiSession` for hosted bots and `BotPlayer` for direct use:

```rust
let mut bot = BotPlayer::new(entity_id)
    .with_difficulty(Difficulty::Hard)
    .with_navigation(navigation)
    .with_sight(sight);
bot.observe_at(observation, now);   // what the bot perceives this snapshot
bot.decide(now);                    // one decision update
let controls = bot.inputs_at(now);  // the commands due, one per input period
```

Observations, controls, and `SimTime` are plain types, and the brain never
sees a socket. The same decisions replay synchronously from a recorded trace
and through the hosted session.

## HUNCH

HUNCH is a small learned policy for the decisions a game already makes. The
official expansion is *Heuristics Under Nearly Constant Hallucination*, though
the player holding the doorway prefers *Hold Up, Need Cover Here*.

Decision zero is the goal arbiter's own choice, what to go for when not
fighting: a pickup that's up or due back, an order, a hunt, ground to hold. For
each candidate the game builds one fixed-order feature vector under a named
schema, `washoi.goal.v0`. It holds the candidate kind, the arbiter's raw
consideration scores, route cost, item facts, and interactions such as health
deficit × route cost. A linear model scores the vectors with one dot product
each, after normalization fitted on training data. A softmax across the
candidates of the same decision is trained to favor the one the human chose.

```sh
# Replay each human seat of a demo through the bot's own perception and planner.
hunch extract match.shagdemo --nav washoi_dm2.json --sight washoi_dm2.sight > records.jsonl
# Fit a model, seeded, on the training matches' records.
hunch train train/*.jsonl --out goal.json --seed 7
# Agreement with the seat's choice: model, arbiter, nearest pickup, and chance.
hunch evaluate goal.json test/*.jsonl
```

Pass the model as `hunch_file`, or `SHAGADELIC_HUNCH_FILE` to the supervisor.
Keep each match's records in one split, chosen before fitting. In `shadow`
mode, the default, the rule still decides and each trace records HUNCH's
choice. In `on` mode HUNCH picks among the candidates that pass the rule's
gates. The loader
checks the schema, the ordered feature names, the dimensions, and that every
weight is finite. The first pipeline result, 48.3% agreement with a seat's next
pickup on held-out matches against the arbiter's 20.5%, is not yet evidence
about people: those seats may be test clients. Human recordings and a reviewed label audit come
first. See [HUNCH.md](docs/research/HUNCH.md).

## Theory

**Same rules as a person.** A bot plays through the game's own client
protocol. It perceives the world through the information a player at its seat
could have, and presses keys a player could press. Decisions read only what the
bot could see, hear, or remember. A demo replayed for HUNCH goes through the
same perception path as a live bot, so no feature can leak the world state a
player never saw.

**The game stays in charge.** The integration owns its rules, physics, and
entity lifetimes, and the lobby owns admission and shutdown. SHAGADELIC owns
scheduling and behavior, so a Washoi command never becomes the runtime's action
type. Another game, with drones or fish instead of soldiers, brings its own
observations, controls, and protocol adapter to the same scheduler.

**Predictable cost.** Work runs inside caller-given budgets and deadlines,
checked against simulation time. One immutable map, navigation, collision,
and model asset is shared per lobby. Each bot keeps its own memory, cooldowns,
and the small scratch state its collision traces write. Rollouts are pure
functions of a start state, so they replay exactly.

**Prove, then move.** A trick is simulated with the game's own movement
code, against the map's real collision, before the bot commits to it, and every
snapshot is checked against the prediction while the trick plays. A
misprediction stops the trick and cools that spot, so it never chains.

**Learn the choice, keep the rule.** HUNCH ranks what the rule already
proposes, so it can't invent a move the integration can't execute. It is
judged in shadow against the rule on identical observations, and earns a vote
only by improving the decisions players see within the lobby's timing budget.

## Licenses

MIT (`LICENSE`).
