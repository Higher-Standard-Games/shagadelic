# SHAGADELIC

Draft architecture for a Rust service. The first integration is Washoi AI players.

SHAGADELIC runs the AI entities for one game session. A host can run it as a supervised child process or embed its Rust library in the game. Washoi is the first integration and uses the child process. Each Washoi bot joins as a headless client and plays through the same input protocol as a human client. The existing TypeScript bots establish the compatibility and feature baseline. Their behavior and code are candidates for reuse where they help reach that baseline.

The first release aims for predictable runtime cost and bots that make better decisions. Future integrations may run several entity types in the same lobby, such as soldiers and imps, or entities with entirely different controls, such as drones and fish. The first release implements Washoi only.

## Implementation status and feature matrix

The current milestone is an **opt-in, playable Washoi bot service**. The bundled Washoi lobby started a Rust child and admitted a ticketed bot in a private Duel on Windows. Separately, live tests confirmed movement and multiple bots in one child. Gameplay parity and production rollout remain open.

| Area | Feature | Status | Evidence or remaining work |
| --- | --- | --- | --- |
| Runtime | Embeddable Rust library | Tested | A fake embedded game drives two entity types at different rates with caller-supplied time. Pause, budgets, and generation-safe cancellation have automated coverage. |
| Runtime | Shared scheduler for Washoi | Tested | Hosted bots share a scheduler and use bounded per-socket input mailboxes. A live test covered simultaneous bots, removal, and replacement. |
| Runtime | CPU worker pool and throughput | Planned | Choose the first background job using profiles. Take the TypeScript baseline and compare CPU, memory, and per-bot input gaps. |
| Lifecycle | Child ownership and host IPC | Tested on Windows | Readiness, idle sessions, shutdown, owner loss, and reaping are covered. Linux has yet to run the process tests. |
| Lobby | Opt-in Rust supervisor and seat tickets | Live verified on Windows | Integrated on the SHAG and Washoi `shagadelic-lobby` branches. A private Duel passed from invite to admission through the bundled lobby. The game-server exit hook and child reaping passed separate tests. |
| Protocol | Washoi join, snapshots, and input | Live verified on Windows | Two-stage local WebSocket admission, snapshot decoding and ACKs, and paced movement work. Fixtures cover missing baselines, appearance fields, and event order. |
| Gameplay | Movement, death, and respawn | Partial | The bot can walk or steer to authored waypoints. A live Washoi fixture confirmed the respawn request and waypoint arrival after respawn. Movement sequences, air steering, pad rides and climbs are ported; run speed and ground steering are tuning. |
| Gameplay | Team and difficulty settings | Partial | The join path accepts a team preference, a seat ticket, and a named difficulty preset. Presets adjust how much health and armor need weighs in pickup scoring. Aim accuracy, reaction time, tracking speed, and prediction skill drive the ported aim controller. Item timing skill blurs remembered respawns. Moods from `BotMood.ts` shift aim and reaction, and each difficulty's mistake rate rolls wild shots, wrong weapons, item glances, and forgotten enemies at the start of a fight. The same rate sometimes takes the goal arbiter's runner-up on a switch. Match sense limits the strategist: an Easy bot reads nothing, and a Hard bot learns which answer pays. The gametype's weight profile, selected by mode, role, and round state from `match_state`, drives goal scoring. Custom difficulty objects and team decision behavior are not ported yet. |
| Gameplay | Map loading, navigation, and traversal | Partial | Includes baked pathfinding, stall recovery, and automatic links to recorded maneuvers. Live DM2 fixtures verify cache jumps, jump pads, and teleporters. On `washoi_dm2`, the bot walked to a takeoff, landed the recorded jump, and reached a separate walking goal. Bots can replan from any graph node and move on from a reached goal. Runtime collision checks remain open. Routes already chain baked jump, pad, and teleporter edges. Recorded maneuvers still end a route, but turning them into graph edges would add no pickups on `washoi_dm2`: the lobby graph reaches all 18 round trip without them, and on a graph with 16 linked recordings, edges add 32 round-trip nodes and no pickups. |
| Gameplay | Perception, combat, and skills | Partial | Pickup goals use the bot's own health and armor, route distance, and locally observed availability. With a sight file, bots see enemies through a static line-of-sight trace, choose a weapon for the range, aim with the ported aim controller, and fire. They strafe, dodge hits, and sidestep incoming rockets while fighting. Two Insane bots on `washoi_dm2` reached a kill in nine of ten live runs within 120 seconds. They notice enemies in a view cone or by sound after half their reaction time, chase one that goes out of sight, switch weapons for rocket, rail, and refire combos, peek from cover at range, take high ground with splash weapons, charge blade blasts, and switch to the smallest hitscan hit that finishes an enemy estimated weak. Weighted hunting, team tactics, and replayable decision traces remain open. Fire waits until the view sent is within 12 degrees of the aim and holds through 25 degrees, and no attack goes out with an ally on the line (`BotFireSafety.ts`). Out of a fight the view rests 10 degrees down, and roaming bots glance into open space and at stale major item spots (`view/BotLook.ts`). Goals come from the ported TypeScript goal arbiter, which scores pickups and orders with the item value, denial, contested item, objective, team, route cost, and danger considerations. It adds commitment, stopovers, failure cooldowns, and item timing in modes that time items. Bots learn which major items an opponent keeps taking, credit enemy takes in their estimates, and slide in with the blade. A strategist answers the match with Recover, Control, Pressure, or Protect lead, and Duel bots hold high ground. |
| Gameplay | Game-mode parity | Live verified on Windows | Server directives guide walking, holding, escorting, planting, and defusing. `tools/bench-washoi-milestone.mjs` plays each mode against SHAG's TypeScript bots at Hard. Duel on dm2: frag gap -0.69 ± 0.87 over 13 matches at `1936415`. FFA on dm2: mean rank 2.34 against 2.66 over 16. S&D on snd1, 24 matches at `7ffa833`: attacking rounds won 65% against 58%, defending 42% against 35%. Amphi on dm2, 8 matches: 4 to 4. Team Deathmatch and Survival are not benched. |
| Rollout | Ratings and match reports | Planned | Rust bot matches run unrated on the integration branches. Rating identity and match report parity are required before enabling rated play. |
| Extensibility | Additional games and protocol adapters | Planned | The embeddable session boundary has tests. Washoi is the only real integration, and the generic protocol adapter interface remains a design sketch. |
| Extensibility | Transport-independent Washoi decisions | Tested | The brain reads typed observations on a simulation clock and returns typed controls. Observation is separate from budgeted decisions. A reference trace replays synchronously and through the hosted session, including a burst under a zero decision budget. Hosted bots fight live, and a private lobby Duel admits and shuts down. |
| Decisions | HUNCH learned tactic and position scoring | Research proposal | First experiment targets Washoi Duel. Capture decision traces during brain separation, then train offline and compare with the current rule in replay and private matches. |

**Initial route recovery is implemented.** A bot stops after 800 ms without enough progress, temporarily avoids the failed segment, and tries an alternative route if it is still near a usable entry. A large displacement discards the active route and allows a fresh attachment. Baked graphs provide up to three reachable goals per entry. When no precomputed route attaches, a bot within 48 units of any graph node plans from that node, using next hops toward each goal that the lobby computes once and shares. The same pickup gates, scores, and blocked-leg memory apply. A bot moves on 400 ms after reaching a plain goal. A bot that momentum carries off the graph plans from the last waypoint it reached, within 128 units, and walks back to it first. From a corner of a walk between two nodes, it finishes that walk to its end node. Farther away, it retries planning every 250 ms. A walk stays within the bot's strongly connected area of the graph, or leaves it one way for an area with more goals. Without that rule, a bot could walk into a pocket whose only exits are jumps and stay there. Authored route files have no graph to plan over. Recovery from off-graph positions and live collision checks remain open.

## Next milestone: agent kinds and hosting adapters

The brain separation gave Washoi one brain with a game-owned contract. The structure still assumes a single kind of AI, a bot that joins as a networked player. A different entity, such as a flying drone in single player, has no place to plug in yet. This milestone makes two independent choices explicit, which kind of agent the entity is and how it is hosted. It proves the split with the only kind that exists today, the WebSocket player.

### Layers today

| Layer | Code | Generic or Washoi | Knows about |
| --- | --- | --- | --- |
| Scheduler and lifetimes | `src/runtime/` | Generic over `GameSession` | Entity keys, spawn and removal, input and decision deadlines, budgets |
| Host control | `src/control/` | Washoi feature | The lobby or supervisor's line protocol |
| Session | `games/washoi/session.rs` | Washoi | One WebSocket client per bot, admission, and the bot's brain |
| Wire adapter | `games/washoi/protocol/` | Washoi | Snapshots, acknowledgments, `Observation` mapping, `Controls` encoding |
| Brain | `games/washoi/behavior.rs` and its modules | Washoi | `BotPlayer` and everything a player bot does |

The runtime already handles several entity kinds. A test drives two fake kinds at different rates. The gap is in the Washoi layer. `WashoiSession` spawns only `BotPlayer`, and only as a networked client. `Controls` is a player's input frame, with a wish direction, view, buttons, and a weapon slot.

### Target layout

```text
games/washoi/
  mod.rs, observation.rs, difficulty.rs, random.rs   shared contract and plumbing
  perception/            senses any kind can use: sight, noticing, enemy estimate
  agents/
    mod.rs               Agent and NetworkedPlayer traits
    player/              the player bot: behavior, combat, movement, strategy, navigation
  hosting/
    net/                 WebSocket hosting: session and protocol
```

Two Washoi-internal traits separate the agent from its hosting:

```rust
/// Every Washoi agent kind: takes observations and decides on its own clock.
pub(crate) trait Agent: Send {
    fn observe_at(&mut self, observation: Observation, now: SimTime);
    fn decide(&mut self, now: SimTime);
}

/// An agent that plays as a networked Washoi player.
pub(crate) trait NetworkedPlayer: Agent {
    fn request_respawn_at(&mut self, now: SimTime) -> bool;
    fn inputs_at(&mut self, now: SimTime) -> Vec<Controls>;
}
```

`hosting/net` drives a brain only through these traits. It builds a concrete `BotPlayer` at spawn and otherwise does not name its internals. Each kind keeps its own action type: `Controls` for the player, and something like a 3D velocity and turret command for a drone. Kinds do not share one action enum. Perception moves out of the player because other kinds need it. Strafing, cover, high ground, the walk graph, weapon slots, combos, and orders stay with the player.

### Adding a single-player drone later

Networked hosting stays the default for networked games, single player included. The game server keeps authority and simulates the world, SHAGADELIC runs the enemy AI beside it, and the client mostly draws. A Washoi drone therefore also plugs in over the network. Embedding is for games that need the AI in process, such as the hockey game.

A drone is not a player, though. `NetworkedPlayer` joins with player admission and sends player input frames, which fits bots in a match but not a server-spawned enemy. A drone needs:

1. **An agent-control channel in `hosting/net/`.** SHAGADELIC holds one connection, or a few, to the server and controls server-spawned NPC entities through it. It receives per-entity observations and sends each entity's actions, with no player admission or seat. This is the multiplexed adapter sketched under the game protocol adapter. It needs a new SHAG server interface for connecting entities other than players. That interface is not part of this milestone, which covers only the player. Snapshot reconstruction, acknowledgments, and backpressure can reuse the player path's machinery.
2. **A drone agent** in `agents/drone/`. It implements `Agent`, plus a hosting trait for its own action type, such as a 3D velocity and a turret command. It has free 3D flight navigation checked against the static sight geometry. It reuses `perception/`, `difficulty.rs`, `random.rs`, and likely the aim controller for a turret.
3. **Kind-aware spawning.** Spawn options name a kind, or the server announces NPC entities on the channel. The hosting session keeps each entity's agent by kind. `src/runtime/` does not change.

An embedded adapter, `hosting/embedded/`, follows when the hockey game needs one. It builds observations from the host's world in process and applies actions directly. Build each adapter against its real user, not before. An abstraction guessed without its second user is usually wrong.

### Status

Implemented. The layout above is in place. `hosting/net` drives the player through `Agent` and `NetworkedPlayer` and names `BotPlayer` only to construct it. The pilot in `control/` and a test do the same. The logical input period moved to `observation.rs`, because hosting paces with it. The trace replay and hosted replay pass unchanged.

### Next step: a generic connection and per-agent wire codecs

`hosting/net/protocol/` still mixes the Washoi connection with what the player speaks over it. Split the two:

| Concern | Today | Belongs to |
| --- | --- | --- |
| WebSocket transport: frames, ping, close, backpressure | `connection.rs` | Generic connection |
| Snapshot reconstruction: baselines, stale ticks, ACK after decoding | `snapshot.rs` | Generic connection |
| Washoi's snapshot event catalogue | `events.rs` | Generic connection (the game's, not an agent's) |
| Command sequence numbers and command ticks | `connection.rs` `send_input` | Generic connection |
| Join, Welcome, JoinAccept, JoinGame, team, seat ticket | `admission.rs` | Player codec |
| Snapshot to `Observation`: whose body, which players | `observation.rs` | Player codec |
| `Controls` to virtual keys to `Command`, respawn requests | `keys.rs`, `send_input`, `InputBatch` | Player codec |

A `Connection<W>` owns the socket, receiver, acknowledgments, sequencing, and pacing for a wire codec `W`:

```rust
/// What one agent kind speaks over a Washoi connection.
pub(crate) trait WireAgent {
    type Join;      // player: name, ticket, team
    type Identity;  // player: player and entity IDs
    type Action;    // player: Controls, and respawn requests

    /// The handshake messages, and how the server's replies admit or reject.
    fn admission(&mut self, join: Self::Join) -> Admission<Self::Identity>;
    /// One reconstructed snapshot, as this agent observes it.
    fn observe(&self, identity: &Self::Identity, state: &GameState<SnapshotEvent>) -> Observation;
    /// One action as wire commands. The connection supplies the sequence and tick.
    fn encode(&mut self, action: Self::Action, commands: &mut CommandSink);
}
```

SHAG defines the messages, because the server has to understand them. An agent kind chooses which server interface it speaks and owns the codec for it. The player's interface exists today, and today's admission, observation, and key code become `PlayerWire`. A drone's `DroneWire` waits for the SHAG interface for non-player entities.

```text
hosting/net/
  connection.rs, snapshot.rs, events.rs    the generic Washoi connection
  player/                                  admission, observation mapping, keys, Controls encoding
  session.rs
```

This is a behavior-preserving refactor. Verify it with both replays (the hosted replay exercises real WebSocket acknowledgments and inputs), the hosted live fights, and the lobby smoke.

Implemented. `hosting/net/connection.rs` holds the generic `Link`: the socket, pings and close, snapshot reconstruction with ACKs, and command ticks and sequence numbers. It runs any `WireAgent`'s handshake, whose `admit` answers each server frame with a frame to send, admission, rejection, or a pass to the snapshot path. `snapshot.rs` and `events.rs` sit beside it. `hosting/net/player/` is `PlayerWire`: admission, the `Observation` mapping, and the movement keys that turn `Controls` and respawn requests into commands and frames. In the code, the trait's join input is the codec's constructor, and actions encode into a list of `Wire` frames. The player's `Connection`, `connect`, `play`, and `exchange` keep their signatures, and `protocol` re-exports the same items, so `control/` and the tests are unchanged. Both replays pass unchanged, three hosted live fights and a direct one reached kills, and the lobby smoke passed three runs in a row. Two earlier runs, one of them with an older binary, failed in the lobby at the map vote, before any child started.

### Verification for this milestone

This is a move plus the two traits, with no gameplay change. It is confirmed with the player over WebSocket:

- `cargo fmt`, `clippy -D warnings`, and `cargo test`, each with and without `washoi`. The trace replay (`behavior/replay.rs`) and the hosted replay (`tests/washoi_hosted_replay.rs`) must match unchanged.
- The live direct and hosted fights on `washoi_dm2`, and the private lobby Duel smoke test with admission and shutdown.
- `hosting/net` uses `BotPlayer` only to construct it. The rest goes through `Agent` and `NetworkedPlayer`.

## Completed milestone: separate the Washoi brain from its client

Start this refactor after the gameplay patch currently in progress lands. Review its changes to perception and behavior before moving those boundaries. Complete the separation before starting another gameplay parity feature. Washoi remains the only game deliverable for this milestone.

The reusable part today is `Runtime<S>` in `src/runtime/mod.rs`. An embedded host supplies time, registers entities, and budgets input and decision updates through `GameSession`. Before the separation, `BotPlayer` in `src/games/washoi/behavior.rs` consumed SHAGGER snapshot state and Tokio time, and the socket task drove observation handling outside the caller's decision budget.

Status: complete. A reference trace from the pre-separation brain (`79af755`) is replayed both synchronously (`behavior/replay.rs`) and through a loopback WebSocket into the hosted session (`tests/washoi_hosted_replay.rs`). `src/games/washoi/observation.rs` defines `Observation`, `Controls`, and `SimTime`. The adapter maps each reconstructed snapshot with `protocol::observe` and encodes `Controls` through the virtual movement keys, sending exact commands unchanged. The brain reads only `Observation` and caller-supplied `SimTime`. The hosted session owns each bot's brain and observes each snapshot on arrival, stamped with the runtime clock. A snapshot wakes a decision, and each budgeted decision plans once over everything observed since the last one. Input updates send paced commands through the socket's one-slot mailbox. Live, two hosted Insane bots in `Runtime<WashoiSession>` reached a kill in five of five runs within 9 to 28 seconds (`tests/washoi_hosted_live.rs`). A private Duel through the bundled lobby admitted a ticketed Hard bot and shut its child down cleanly. `BotPlayer::observe_at` only stores an observation: the latest bodies, players, and pickup mask replace the previous ones, and its events join a queue tagged with their snapshot tick. The queue holds up to 4,096 events, and a longer burst drops the oldest and counts them in `dropped_events`. `decide` plans on the latest view with every queued event in order. `inputs_at` returns `Controls`, exact for recorded traversals and precise approaches. A brain test feeds three observations, with a death and a respawn among them, before one decision, and checks the order and the latest view. The hosted replay delivers a death and a recovery in one burst under a zero decision budget, then allows one decision, which plans nothing until permitted and then acts on the death the latest view no longer carries. After this rewiring, hosted live fights reached a kill in five of five runs within 15 to 23 seconds, and the lobby smoke passed three of four runs. The failed run printed no output and was not captured.

Implement and verify the changes in this order:

1. **Capture current behavior.** Use existing Washoi fixtures and add a short trace with ordered observations, elapsed times, a known random seed, and expected actions. Include a one-tick event and an exact movement sequence so event loss or a shifted keypress is visible.
2. **Give the brain a game-owned interface.** The Washoi integration derives a typed observation limited to what that bot can perceive. The brain accepts it with caller-supplied simulation time and produces a typed Washoi action. Keep combat, skill priority, movement physics, and per-bot memory inside Washoi. Keep snapshot reconstruction, ACKs, sequence numbers, sockets, and input encoding in the adapter. Replay the trace against the production brain in an ordinary synchronous test. Replaying from the same initial state must yield the same actions.
3. **Make the scheduler responsible for decision work.** Receiving a snapshot updates the latest observation, preserves ordered transient events, and can wake a decision. A due decision selects the next action. A due input update advances or emits the selected controls at Washoi's cadence. Test a burst of observations with a zero decision budget, then allow one decision and verify it sees the pending events in order. Check input pacing and exact maneuver progression across those ticks.
4. **Verify both entry points.** Keep the existing embedded runtime tests building with Washoi disabled. Run the production Washoi brain against the replay adapter and the live socket adapter, and exercise lobby admission, movement, traversal, combat, and teardown. Use these two callers to settle the smallest adapter interface they actually share. A second game and a runnable demo are outside this milestone.

### Candidate shared waypoint format

After the brain separation, evaluate a versioned waypoint format using the authored DM2 routes and the existing baked DM2 graph. Start with the data another game could interpret: map identity, coordinate frame and units, node identifiers and positions, ordered route waypoints, and directed links with optional intermediate points. An embedded game should also be able to construct that data in memory. A file is one input to the library.

Washoi owns the meaning and eligibility of a traversal, including jump speed, trigger volumes, teleporter exits, pickup goals, and recorded controls. Associate those fields with the relevant node or link in a Washoi extension. A reader that cannot execute a required traversal must exclude that link from planning. A shared graph reader should never silently turn it into a walking edge.

Keep the current Washoi files and TypeScript generation tools usable. Prototype a reader or conversion from the existing assets before committing to a new on-disk schema. Compare waypoint order, directed connectivity, and executable maneuvers on DM2, including a one-way edge and a teleporter. Adopt the shared format once those checks pass and the basic graph can be read without enabling Washoi.

## HUNCH: a small learned decision policy

**HUNCH is the official name.** Today's expansion is *Heuristics Under Nearly Constant Hallucination*. The player holding the doorway prefers *Hold Up, Need Cover Here*. The player who just respawned insists it means *Health Under Nine? Challenge Him.*

[HUNCH.md](HUNCH.md) is the build plan: decision zero is the goal arbiter's own choice, with its consideration scores as features and labels from what human players took next.

HUNCH scores a small set of executable choices at a scheduled decision. Each game defines its own observations, tactics, candidate positions, and execution rules. The reusable Rust library loads weights and scores feature vectors synchronously. An embedded simulation and a supervised Washoi lobby can use the same scorer. During the brain refactor, capture what the existing rule saw, considered, and chose. Model training and rollout follow that milestone.

### First experiment in Washoi Duel

Score each **tactic together with the position or target that makes it viable**. Washoi proposes candidates using the bot's own perception, memory, and reachable navigation links. Include the current position. Keep the candidate set small, retain the current plan among eligible options, and reconsider on a decision deadline or a meaningful event. Washoi still owns interruption rules and exact input sequences.

| Candidate | Evidence the bot can use at decision time |
| --- | --- |
| Attack here or move to attack | Weapon and ammo, effective range, current sightline, expected exposure on the way to a firing position, and time until it can shoot there. |
| Break contact or seek resources | Own health and armor, first reachable cover, travel time, remembered pickup availability, and whether the route crosses the last known line of fire. |
| Pursue after losing sight | Age and uncertainty of the last sighting or sound, reachable intercept positions, and the resources or ground surrendered by chasing. |
| Deal damage from a losing position | Chance of escaping and denying a frag, chance of landing another shot, and whether the opponent is likely to recover before the next encounter. |

For example, at nine health a bot can still have a good reason to fire. If the only exit crosses the opponent's sightline and the bot recently saw the nearby health pickup taken, one more hit may give it an advantage after respawn. With cover a step away or health available to the opponent, the same shot can be a poor trade. On death, clear the firing plan, target lock, and movement sequence. Within the same match, retain permitted evidence such as a perceived hit and its time. Decay confidence as the opponent has time to move or heal. At respawn, score fresh options from the new position and loadout. Awarding a fixed bonus for damage before dying would miss both the cost of the frag and the value of survival.

### Training data and tools

Inspect the earlier Washoi experiment in `src/games/fps/models/{tactical,combat}/` and `models/training_stats.json` under the FPS game directory. Verify its feature ordering, labels, and provenance before considering its weights. The existing `training_demo` and `bot:learn` workflow supplies movement recordings. Tactical supervision needs decision moments from human Duel play.

| Proposed offline CLI operation | Contract |
| --- | --- |
| `hunch extract` | Use the live integration's observation and candidate builders against recorded player POV. Emit one record per decision with match, life, tick, schema version, eligible candidates, and their features. Flag missing POV evidence. Keep later outcomes in separate fields for review. |
| `hunch inspect` | Show a clip and its proposed positions. Label the player's tactic and destination separately. Allow unclear intent and a missing candidate. Keep those cases for diagnosis and exclude ambiguous choices from supervised fitting. |
| `hunch train` | Fit on reviewed human choices. Split by whole match before normalization or tuning, record the split and seed, and retain untouched matches for final evaluation. Bot traces provide a rule baseline and examples for disagreement review. |
| `hunch evaluate` | Compare the model and rule on identical observations. Report tactic agreement, position preference within a tactic, missing candidate rates, and results for rare losing-position choices. Review clips across death through the next encounter, including opponent recovery and the frag conceded. A later kill alone cannot establish that the earlier trade paid off. |

### Rust implementation and rollout

Start with a linear candidate scorer trained from scratch. For each eligible candidate, build a fixed-order vector containing a tactic indicator, candidate-specific features, and selected interactions such as health deficit times route exposure. A feature identical for every candidate cancels out of their ranking unless it interacts with a candidate feature. Compute one dot product per candidate. During offline training apply a numerically stable softmax across choices from the **same decision**, minimize negative log probability of the reviewed choice with L2 regularization, and update weights with seeded minibatch gradient descent. This needs vector arithmetic on a CPU. Try a small hidden layer only if systematic errors on held-out matches justify its additional complexity.

Expose a game-independent operation such as `model.score_into(candidates, scores)` with caller-owned buffers. Washoi maps scores back to eligible tactic and target IDs and breaks ties reproducibly. Store the model version, feature schema and order, normalization parameters fitted on training data, and finite weights in a versioned artifact. Validate dimensions and schema at load. Share one immutable model per lobby and retain the existing decision rule as a fallback. Verify a known trace produces the same scores in the trainer and runtime.

First run in shadow mode: log what HUNCH would choose while the existing rule plays. Review disagreements for missing positions, poor rankings, or execution failures. Then compare the two in private Duels, including blinded human judgments, frequency of plan reversals, successful disengagements, and pressure retained after respawn. Measure decision latency and input gaps at representative bot counts. A model earns adoption by improving the decisions players see within the lobby's timing budget.

## Washoi process ownership

The host starts one SHAGADELIC child process per game lobby and supervises it for the life of that lobby. Here, a lobby is the playable session assigned to a game-server instance. A lobby service that coordinates several sessions starts a child for each session. The document uses “host” for the component that owns the child, even if the lobby service and game server are separate processes.

1. The host starts the child with a single lobby identity, the selected integration, a game endpoint, and a private control channel. The child validates configuration and reports readiness. Each bot reports admission separately.
2. The host requests spawns and removals. Each entity has a host-assigned ID and moves through pending admission, active, and ended states. A duplicate spawn request for the same ID returns its existing status. Removing a pending entity cancels admission and closes any connection that finishes opening afterward.
3. An empty lobby can stay ready to accept new bots. A single bot socket closing ends that entity's session. The host decides whether to replace it. The adapter reports game-session termination through a verified session-level signal, and the host shuts down the child when the game server ends.
4. Lobby teardown, owner-channel closure, or confirmed game-session termination initiates shutdown. The child stops accepting spawns, cancels pending joins and background jobs, closes bot sessions, and exits. The host enforces a bounded graceful shutdown and reaps the child, terminating it if cleanup stalls.

In the first integration, keep Washoi's existing host-side seat assignment, admission ticket issuance, and rating storage. The process boundary needs equivalents of the current runner's add, remove, admission result, disconnect, rating reference, and match report messages, plus readiness and shutdown. Carry lobby and request IDs where needed to correlate results. Use a private parent-child channel with explicit framing and a version handshake. Inherited stdin/stdout pipes with versioned JSON records are the initial candidate for Node-to-Rust control on Windows and Linux. Reserve stderr for diagnostics and validate parent-death behavior on both platforms. Keep credentials out of command lines and logs.

## Architecture

| Component | Owns |
| --- | --- |
| Host control | Startup config, readiness, spawn/remove requests, lifecycle events, report delivery, and shutdown. |
| Lobby runtime | Entity IDs and lifetimes, scheduling, bounded background work, cancellation, and metrics. |
| Game integration | Entity type catalog, typed observations and actions, game rules, and shared map or world services. |
| Protocol adapter | Connections, handshakes, wire encoding, incoming message order, and translation to and from the integration's typed game data. |
| Entity behavior | Per-entity perception and memory, goal selection, skill selection, and action execution. |

### Embedded games

Keep the scheduler and entity lifecycle in a library target in the same Cargo package. The Washoi binary owns host control, process lifetime, and driving the library with a monotonic clock. An embedded game owns the library instance and advances it from its simulation loop. The library scheduler accepts time and work budgets from the caller. It does not sleep or start a timer, executor, or worker thread implicitly.

An embedded game such as ice hockey can provide typed observations directly and consume typed actions for the same simulation. The game remains authoritative over physics, rules, and entity lifetimes. For each fixed simulation tick, it can supply the current world view, run due AI work, apply the returned actions, and advance physics. Use simulation time so pausing freezes AI deadlines and replays can advance one tick at a time. Start with bounded inline updates. If embedded games later need expensive background jobs, let the host opt into workers and apply their results at simulation tick boundaries.

Use the game-session boundary for both forms of hosting. Washoi currently implements admission and wire transport in its concrete socket client. The proposed `GameProtocol` interface below will be evaluated during the separation milestone. An embedded integration can register entities against its local simulation and exchange observations and actions by direct calls. Keep game-specific types in the integration and let the first real embedded game inform the public API.

The runtime calls into one selected game integration per lobby. The integration registers its supported entity types and supplies its protocol adapter. Many entity types can share an adapter. Washoi uses one connection per bot. An adapter for another game could multiplex several entities over one link and route game updates by entity ID. The adapter interface should express entity admission, observation delivery, action submission, and session loss without exposing sockets or assuming a connection count.

The adapter plugin boundary is a Rust interface with statically linked implementations in the first release. The selected integration owns the concrete types for world updates, entity observations, and actions. Where types differ within a game, the integration can route them internally. Keep the runtime's interface at the game-session boundary so a Washoi player action never becomes the common action type. Replacing a wire protocol for the same game can then preserve behavior code if the new adapter supplies equivalent observations and accepts equivalent actions. Dynamic loading and a stable plugin ABI are future design decisions.

For Washoi, decode snapshots into a world view, derive observations each bot could legitimately perceive, and pass those observations to its brain. A behavior returns a typed Washoi action that the adapter encodes as player input. Map geometry and navigation can be shared across the lobby. A bot's awareness, target history, reactions, and cooldowns belong to that bot. Sharing perception computations must preserve those boundaries.

## Rust interfaces

These sketches define ownership and data flow. Names and payload fields can change as the first Washoi fixtures pin down the contract.

### Runtime and game session

Use a statically linked integration selected when the runtime is created. The runtime owns AI entity lifetimes and scheduling. It calls the integration for each due update and passes a caller-supplied timestamp that increases within the session. The integration owns game data and selects the concrete behavior for each entity. Keep the runtime generic over the selected game session so the scheduler can use an in-process fake game in tests.

The session boundary covers four operations.

| Operation | Contract |
| --- | --- |
| Begin or end admission | Begin with an entity key and typed spawn options. Removals use the same key and cancel pending work. Admission results are delivered as events. |
| Drain events | Return a bounded batch of lifecycle and game events. Lifecycle events carry the entity key or explicitly identify the whole session. |
| Advance entity | Run input and decision work when each is due. The session supplies the next deadlines and can request an earlier decision when a relevant game event arrives. |
| Begin shutdown | Stop admission and start closing the adapter. The runtime waits for cleanup within the host's shutdown budget. |

An entity key pairs the host-assigned ID with a spawn generation. The runtime allocates it before admission starts and checks it on every asynchronous result. The Washoi game session maps the key to a server-issued player ID after admission. The runtime only needs the player ID if the host contract requires it in a report.

### Game protocol adapter

The adapter presents typed commands and events to the game session. The same interface can serve a socket-per-bot protocol, a multiplexed connection, or a test adapter that replays a fixture. Use an associated type for each game-specific payload.

```rust
trait GameProtocol {
    type Join;
    type PlayerId;
    type Update;
    type Input;
    type Request;

    fn try_send(
        &mut self,
        command: ProtocolCommand<Self::Join, Self::Input, Self::Request>,
    ) -> Result<(), SubmitError<ProtocolCommand<Self::Join, Self::Input, Self::Request>>>;

    fn try_recv(
        &mut self,
    ) -> Result<Option<ProtocolEvent<Self::PlayerId, Self::Update>>, AdapterClosed>;
}
```

`ProtocolCommand<Join, Input, Request>` carries `Connect { key, join }`, `Disconnect { key }`, `SubmitInput { key, input }`, `Request { key, request }`, or `Shutdown`. `ProtocolEvent<PlayerId, Update>` carries `Admitted { key, player_id }`, `AdmissionFailed { key, reason }`, `Update { key, update }`, `Disconnected { key, reason }`, `SessionEnded { reason }`, or `ShutdownComplete`. The first adapter uses per-entity updates. A future adapter can add a session-wide update if its server publishes shared data that way. `SessionEnded` requires authoritative session-level evidence. `ShutdownComplete` means connection tasks have finished. An unexpected adapter closure is an adapter failure.

Both methods are nonblocking. The adapter exposes readiness through the event loop or a channel wakeup. Its network tasks own sockets and perform handshake, decoding, and writes. Submitting a command means the adapter accepted it for processing. Admission succeeds only when the corresponding event arrives. Use bounded queues with reserved capacity or a dedicated path for cancellation, shutdown, and terminal events. On full input queues, apply the game's ordering and pacing rules and surface sustained backpressure as a connection failure. Reliable events retain their order and either reach the consumer or fail the affected session. Keep the host control messages and their versioning independent of this interface.

On a full queue, return ownership of the rejected command in `SubmitError::Full(command)`. This preserves a pending join ticket or exact movement input across retries. The producer commits its maneuver cursor when submission succeeds. The adapter assigns the wire sequence once, in send order. A failed connection terminates its pending inputs. Reserve capacity for removal and shutdown and suppress queued commands for a removed generation.

For Washoi, the first implementation is `WashoiProtocol`. On WebSocket open it sends `Join` with the bot flag and optional admission ticket. `Welcome` supplies server metadata. The first `JoinAccept` seats the client as a spectator and triggers `JoinGame`. The acceptance received in the joining state establishes the playing identity and emits `Admitted`. A close or rejection between those accepts fails the pending admission. Cover that case and the successful exchange with fixtures from `BotClient.ts` and `GameServer.ts`. The game integration chooses team preference and when to request respawn. The adapter owns wire encoding, acknowledgments and command sequence numbers.

### Snapshot correctness and Rust reuse

Start the compatibility spike with SHAGGER's `shagger_shared/src/net/message_codec.rs` under `crates/`. It has join, join-game, command, and acknowledgment encoders. SHAGGER also has `crates/shagger/src/network/client/snapshot_receiver.rs`, and the Rust Washoi client supplies the game-specific `WashoiEvents` codec in `src/shared/net/snapshot_events.rs`. Check visibility, the pinned revision, and the resulting headless dependency graph before choosing how to share these implementations.

Allocate one receiver per connection. The TypeScript and Rust receivers retain 128 snapshot baselines, reject stale ticks, and skip deltas with missing baselines. ACK only successfully reconstructed snapshots. Keep history through player death and clear it on reconnect. Extract ordered one-tick events before replacing any pending observation with a newer one. An event can disappear from the next full world view even when both frames decode correctly. A parity fixture should feed a full snapshot, dependent deltas, a duplicate, a missing baseline, and a recovery full snapshot through both receivers. Compare accepted ticks, ACKs, and delivered events.

### Washoi bot player

Code layout under `src/games/washoi/`:

| Path | Holds |
| --- | --- |
| `mod.rs`, `observation.rs`, `difficulty.rs`, `random.rs` | Public exports, the shared contract (`Observation`, `Controls`, `SimTime`, `INPUT_PERIOD`), difficulty presets, per-bot random draws |
| `perception/` | Senses any agent kind can use: noticing enemies (`mod.rs`), static line of sight (`sight.rs`), the enemy health estimate (`estimate.rs`) |
| `agents/mod.rs` | The `Agent` and `NetworkedPlayer` traits |
| `agents/player/` | The player bot: `BotPlayer` (`mod.rs`) and its trace replay, `combat/`, `movement/`, `strategy/`, `navigation.rs` and `navigation/` |
| `hosting/net/` | WebSocket hosting: the session (`session.rs`) and the wire adapter (`protocol/`) |

Start with a concrete `BotPlayer` type in the Washoi integration. It is the simulated player and has an entity key, a proficiency profile, perception memory, an active plan, aim and maneuver execution state, and a reproducible random seed. The Washoi session owns these players after admission, keyed by the same entity keys the runtime tracks for lifecycle and scheduling. The game session keeps map geometry and navigation in shared data and supplies per-connection game updates to observation building.

```rust
impl BotPlayer {
    fn observe(&mut self, observation: &Observation, now: SimTime);
    fn decide(&mut self, context: &DecisionContext<'_>);
    fn next_input(&mut self, tick: InputTick) -> Option<PlayerInput>;
}
```

These are method signatures for the design. `observe` updates perception and invalidates plans on events such as death or map change. `decide` picks or revises the active plan using perceived game state and the available world services. `next_input` advances the plan and aim for one logical input tick and emits a concrete Washoi movement, view, button, and weapon selection. It can skip input while the player is awaiting spawn. The Washoi session owns input pacing and bounded catch-up. Exact maneuver sequences produce their inputs in order across logical ticks. Each session dispatches those paced inputs to the adapter. This keeps AI plans and sequences usable with a replay harness or a different Washoi wire adapter.

The first playable behavior can choose a simple movement plan and exercise the input path. Add goal scoring, skill ranking, and channel claims within `behavior/` as features are ported. An additional Washoi entity type can choose a different behavior within the same integration. A different game defines its own entity state, observations, and action model.

## Behavior and skills

**Escaping slime and lava.** `navigation/liquid.rs` ports `LiquidEscape.ts`. A bot with slime or lava at its feet, waist, or chest jumps for the nearest dry graph node within 1,500 units and no more than 64 below, preferring one in sight, and keeps its aim and fire. Sight files at version 2 (`SHAGSGT2`) carry the slime and lava world brushes after the version 1 geometry. `Sight` still reads version 1, but older binaries cannot read version 2. The shipped `washoi_dm2` file is version 1 and has no liquids. Of the maps checked, `washoi_dm3` and `washoi_dm7` have lava. Live, the bot recognized the baked lava and jumped. Whether Washoi's server hurts a body there could not be confirmed, because its collision world leaves out non-solid brushes.

**A second baked map.** `washoi_dm3` has been baked with 101 nodes, 630 walks, and 33 maneuvers. The files are scratch output, not committed. To rebuild them, run these from a SHAG checkout whose `.cache/bots/maps/` holds `washoi_dm3.json`: `node node_modules/vite-node/vite-node.mjs <SHAGADELIC>/tools/bake-washoi-walk-graph.mjs -- --shag-cache washoi_dm3 <dir>/washoi_dm3.json` and `node node_modules/vite-node/vite-node.mjs <SHAGADELIC>/tools/bake-washoi-sight.mjs -- washoi_dm3 <dir>/washoi_dm3.sight`. Point `SHAGADELIC_NAVIGATION_DIR` or the benchmark's `--nav` at `<dir>`.

**Slides and hop chains.** A walk at 375 u/s or more crouch-slides down a straight, plain lane (`navigation/slide.rs`, after `RouteCrouchslideSequence`), or slides onto a pickup on its last leg (`ItemSlideApproachSequence`). From 320 u/s, a bot with `strafeJumpSkill` 0.35 or more strafe-jumps down an open lane (`navigation/hop.rs`, after `RouteStrafeJumpSequence`), re-hopping on each landing while the route has room. It sends human-possible keys: the view turns toward the hop's wanted view at 2.5 + 4.5 × skill radians a second, and the keys are the chord nearest the wanted wish in that view. Neither starts during a fight, and a running hop yields to one outside its takeoff. In traced Duels against the TypeScript bot at `3b59971`, hops raised the Rust bot's airborne share from 20% to 25% and its 99th-percentile speed from 427 to 521 u/s. The TypeScript bot is airborne 42% of the time and moves at 375 u/s or more in 42% of moving samples, against 9% here, so slides seldom start. Bent hop chains and steadier ground speed remain open. `navigation/rollout.rs` steps Washoi's own `MovementProfile` (the `washoi_movement` crate) ahead of the bot, as TypeScript's rollouts do. `navigation/ledge_slide.rs` ports `planLedgeSlideJump` over it. Running at 375 u/s or more toward a drop or jump, the bot crouches into a slide and jumps at the ledge. It does so only when the whole script stays grounded to the jump, keeps out of slime and lava, and lands within 96 units across and 64 up or down of the route's landing. To keep the keys a player's, the script runs forward along the view, and only when the walk heads within 8° of it. TypeScript runs it along the walk under any view. The bot has no map collision world yet: `simulate` needs a `CollisionWorld`, and SHAG's sweep backend is in `shagger`, not `shagger_shared`. The planner is tested over boxes, and nothing calls it live. The sweep has since moved to `shagger_shared::physics` (shagger #36), with its BSP reader. `tools/bake-washoi-collision.mjs` writes a map's collision for the bot to load as `collision_file`, found through SHAG's pk3 index: `node node_modules/vite-node/vite-node.mjs <SHAGADELIC>/tools/bake-washoi-collision.mjs -- washoi_snd1 <washoi_snd1.collision> --expect-sha <bsp_sha256>`. The file is `SHAGCOL1`, a JSON header with the map and the full BSP's SHA-256, then the BSP with the lumps collision never reads emptied. `washoi_snd1`'s 96 MB BSP, most of it lightmaps, becomes 18 MB. Built from the full and the trimmed BSP, the sweep answered 20,000 traces, slide moves, and content probes identically. A BSP names no map, so the child refuses a file whose `bsp_sha256` differs from its navigation's. With no walk graph, it refuses any. The supervisor passes `<map>.collision` from the navigation directory as `collision_file`. The child parses it once into the session's `MapAssets`, and each bot gets its own `SharedCollisionWorld` over a clone of the sweep: a trace writes the sweep's scratch state, and each bot moves lifts and doors from its own snapshots. Until shagger shares the parsed map between clones, each clone costs about 23 MB on `washoi_snd1`. A live four-bot S&D plant run with the collision loaded passed. The ledge slide jump now plays live. Its start gates are TypeScript's: Hard or better (`strafeJumpSkill` 0.55), no fire, not fighting or chasing, a probe every 31 ticks, and the 1.5 s slide cooldown. It runs when the walk's leg, or the one after it, drops more than a step. The bot's walk lets sprint go near a drop, so the run is judged by the planner's 375 u/s instead of the walk's sprint key. A probe is spent only once the edge is within range. While the script plays, the controls are exact and the mover is `ledge`. It yields to a fight until its jump, then runs to the landing. Every snapshot must lie within 24 units, and 80 u/s, of a state the inputs sent so far could have reached, or the script stops and that ledge waits five seconds. A stale view stops it too. In traced Hard Duels on `washoi_dm2`, though, no script started. Rust's own braking before a drop leaves the bot at a median 167 u/s when the edge comes within the 224-unit probe, and the few probes at speed faced drops of over 160 units. Braking is not the cause. A trial that held sprint and skipped the brake toward any drop a script could take (Hard, out of a fight, at most 160 down, no slime or lava) held 269 times in two traced Duels. Approach speed stayed at a median 129 u/s, every probe that reached the planner was under 375 u/s, and no script started. In eight Duels against the TypeScript bot, deaths it didn't cause went from 1 to 3, within noise. The bots reach dm2's short approach legs slow: from turns, and from view glances that make the keys a chord, held to 320 u/s. Ledge slide jumps stay rare until the general run speed matches TypeScript's, and the trial did not land.

**Implemented flying starts.** `navigation/flying_start.rs` ports `sequences/FlyingStart.ts`, and the learned-route traversal runs it. A learned route's script starts from standing on its mark: a sprint run-up, then the recorded trick. With the map's collision, the bot replays the run-up once from the mark. The replay uses Washoi's whole movement and the map's own jump pads and teleporters (`MapCollision::triggers`, from the BSP's trigger table). Then, if the ground is walkable to an entry point 192 units behind the mark and from there to the mark, it sprints in along the route's line. It joins the script on the run at a run-up state within 16 units, 60 u/s, and 10° of where the server will have it 3 ticks on. The rest of the script must land, simulated from the bot's own state, for leads of 2 to 5 ticks, and, unlike TypeScript, with the start shifted 6 units to either side. Snapshots quantize the bot's state, and the run-in never heads exactly along the route. Without a join, it falls back to stopping on the mark. A live fixture spawned the bot 80 units behind a mark on washoi_dm2, with a graph from the spawn to the linked route. Two recordings with 18-tick run-ups joined at state 14 and landed in about 270 ticks, against 300 to 900 from the mark. `wdm2_ra_jump_2@3482` landed every join. `wdm2_ra_jump_2@6162`, whose landing lies 30 units from a teleporter, landed 2 of 7 joins without the side margin and 4 of 6 with it, and every start from the mark. `wdm2_ra_jump@6449` has no walkable run-in, so it starts from the mark. In ordinary Duels the arbiter seldom picks a learned-route goal, so flying starts are rare there.

**Virtual movement keys.** Ordinary bot steering selects forward, back, strafe, or a diagonal in the sent view's coordinate frame. Each client holds a chord for at least three logical input ticks through small turns. Stops and reversals take effect immediately. The encoder produces the world-space direction and movement button bits from the same keys and preserves the other action bits. Objective routes and, since `815cd19`, roaming routes hold sprint on long, level grounded legs and release it on approach. Speed depends on it. Washoi's `MovementProfile.simulate` gives `runSpeed`, 400 u/s, only to forward alone with sprint, while not firing, aiming down sights, or crouching. Every other key combination gets `maxSpeed`, 320. The `815cd19` commit message is wrong to call sprint input parity only. A bot whose view is turned off its walk sends a diagonal chord and stays at 320, so moving the view matters as much as the key. **Bots send only inputs a person could.** TypeScript's `BotClient._sendInput` moves the feet along a world direction from its `moveYaw`, with FORWARD and SPRINT held, apart from the view yaw it sends. It sprints at 400 while glancing, which no client can do. The user chose, on 2026-11-22, to keep Rust keys relative to the sent view and to close the gap on the look side instead. While sprinting, `look::keep_sprint` holds a roaming glance within 20 degrees of the run, inside the forward key's 22.5 degrees. Strafe-jump chains follow the same rule: the sent view is the hop's heading, so the keys are what a strafe-jumping player holds. Precise takeoff approaches and recorded traversal keep their existing command directions and timing. Because the keys follow the sent view, the view must keep to the run. A walk makes for the point 64 units along its level run, as TypeScript's `followRouteLine` does, so corners turn over the run into them. Running with nobody about, the calm aim settles about three times as fast as its fight pace, at up to 900 degrees a second. Together they took dm2 Duel moving time at 375 u/s or more from 21.7% to 30.0% (WORK_SPLIT.md).

**Gametype orders.** A bot accepts its own `move_to`, `hold_area`, and `defend` orders, checks their server-tick expiry, and follows any lane waypoints through the shared walking graph. A lane point counts as passed within 128 units across and 72 up or down, TypeScript's `VIA_RADIUS` and `VIA_HEIGHT`. Held to 48 and 40, an S&D carrier that dropped off a ledge 125 units past a point climbed back to touch it. Unlike TypeScript, which starts each new order at the lane's first point, a bot joins a lane at its nearest point, or at the next one once past it. S&D gives the carrier and its escorts the same lane from the attack spawn. A bot that took it up again mid-map after fetching the dropped bomb walked back to the spawn. It stops in the target radius and can look at the server's watch point. S&D's `escort` order tracks an allied carrier from the current snapshot and replans as the carrier moves. A stalled route avoids the blocked directed edge for five seconds and can return to the last confirmed node when it stalls on the first straight leg. Missing targets and unreachable positions fall back to ordinary movement, with a bounded retry rate for pathfinding. On arrival, `move_to` stands still, so plants and defuses hold their spot. `hold_area`, `defend`, and `escort` patrol instead, after `BotOrders.guardPatrol`: the bot runs between points on two rings around the spot that it can walk to straight, preferring ones that see the order's watch point. It watches that point with a slow sweep, and strafes across it when no point is walkable. TypeScript watches a graph way into the spot when the order names none. Here the walk direction sets the view. A guard whose fight drifts past the order's `leash` walks back while it keeps aiming and firing, as `applyLeash` does. In three live four-bot S&D runs on `washoi_snd1` with these rules, attackers planted within 39 to 79 seconds.

The live plant test is flaky, and diagnostics that log the bomb, every player, and its order every two seconds explain why. On `main` at `9507ab5`, it passed 5 of 8 runs. In the failures, bots stood still under an order for 32 to 48 seconds. A carrier left partway up one of the east stairs, whose baked walks climb through corners, was more than 48 units from any node, so its plant order could not attach a walk. A fetcher pushed onto open floor or a platform more than 128 units from a node could not either. An objective walk now starts partway along a walk edge within 48 units of its path at that height, and finishes that edge first. The straight connector to a node reaches 256 units, still sampled every 16. With both, no bot under an order stood still for more than ten seconds in six runs. The test still passes about half the time, because combat decides the round: the carrier often dies on the east route, and fetching the bomb back takes longer than the 80-second window. After a plant, the gametype chooses which defender defuses (`snd_defuse`) and which guard (`snd_guard_defuse`). In a live four-bot round on `washoi_snd1`, it sent the defender about 2,500 units away while the guard stood at the bomb, and the fuse ran out before the defuser arrived. The bots followed their orders. That choice belongs to the server, and TypeScript bots receive the same orders. No Washoi gametype issues `hunt` orders.

Decision traces (`SHAGADELIC_TRACE_DIR`, with each decision's mover) showed that the remaining plant failures at `723ea62` were the order walk itself, not fight moves. The test passed 1 of 4 runs there. Three faults sent carriers back the way they came. The lane-point radius was too tight, a lane restarted at the attack spawn, and a two-way link was read in one direction (see the gametype orders above). Partway along a two-way link, the bot stands on both of its edges. The planner took the nearer, or the first, and on the east drop a carrier that landed partway down replanned up the ledge, fell again, and detoured round site A once the stall blocked the edge. An objective walk now seeds its search from every edge the bot stands on, and from each one's reverse, which bakes up to 48 units apart. A route planned from a ledge right over its first node had no leg to step off along, so the bot stood there for 36 seconds; it now steps off toward the next waypoint. Unstick no longer fires while the server holds every body, in `MatchPhase.Countdown` (the freeze before each round) and `RoundEnd`. With these fixes, the test passed 5 of 8 runs. No carrier stood still under a move order outside a held phase. In the failures, the carrier died at a site and the next round could not finish inside the 80-second window.

With the map's baked sight geometry, objective routes can attach to nearby graph nodes and finish at off-graph targets such as a planted bomb. These connectors are limited to 128 units and require sampled static clearance and floor support. Washoi still simulates player movement and collisions on the server. Moving platforms and dynamic blockers are outside the static check.

Keep these three inputs to decision-making distinct.

| Concept | Washoi examples |
| --- | --- |
| Capability | The entity can track a target, dodge, or execute a crouch-slide sequence. |
| Proficiency | A profile sets reaction delay, tracking accuracy, movement reliability, item timing, and tactical awareness. Existing difficulty presets map to these values. |
| Decision policy | The bot evaluates whether to contest a pickup, hold position, disengage, or choose another goal. It picks skills that can execute the chosen goal under current conditions. |

The first Washoi behavior should preserve the useful parts of the TypeScript goal arbiter and input claims. Give each candidate a stable ID, eligibility gates, and a score with named factors. Reject inapplicable actions before scoring. Rank the eligible goals by their current value, with stable ties and a commitment window to limit indecision. For the selected goal, rank the applicable skills by expected effectiveness, risk, execution cost, and the bot's proficiency. A skill declares its prerequisites, input channels, and completion or interruption conditions. Rank only candidates the bot can actually use. A full sorted list is useful in a decision trace, while the hot path can select the best candidate in one pass when it needs only a winner.

A maneuver can run over multiple ticks. It retains its action channel claims until completion or an allowed interruption. For Washoi, independent view, movement, and button channels let a bot track an opponent while finishing a movement sequence. Define what can preempt a claim, what happens when a target or route becomes invalid, and what fallback action the entity takes. Keep Washoi-specific skill data and policies in the Washoi integration. Reuse a selection or execution primitive in the runtime once a second behavior provides evidence that the primitive is shared.

**The goal arbiter is ported.** `strategy/arbiter.rs`, `considerations.rs`, `items.rs`, and `weights.rs` port `goals/GoalArbiter.ts`, the considerations, the shared item grants, and the gametype weight profiles. `strategy/planner.rs` ports `ItemGoalPlanner`, `ItemGoalSource`, and `selectMandatoryGoal`. Candidates are the pickups the bot believes up, a major item due back soon where the mode times items, and the orders addressed to it. Session B's `strategy/candidates.rs` offers the pickups and orders. Walk costs come from `pickup_costs`, attached once per decision with the roaming planner's chain. An `interrupt=never` order is taken without arbitration. Until a `match_state` names the gametype, orders keep their own choice, because the default profile gives them no weight.

The planner adds failure cooldowns: a pickup circled for a second, a walk that has not shortened in four seconds, or a pickup the follower cannot plan to is left for five seconds. The follower then chooses freely for two seconds. `SHAGADELIC_GOAL_ARBITER=verbose` logs each new choice, as `BOT_GOAL_ARBITER=verbose` does in TypeScript. The roam fixture allows 40 seconds instead of 25, because the arbiter may send a fresh bot across the map for the best item left. It passed 4 of 5 runs, against 3 of 3 for the old nearest-value pick. The failure oscillates between nearby pickups and gives up on stalled walks.

**The strategy layer is ported, except learned priors.** `strategy/opponents.rs` credits major items heard taken within 2,500 units, or seen vanishing beside an enemy (in a Duel, to the one opponent). An item taken twice in three minutes is contested, from Hard difficulty up, and an armed enemy makes the bot cautious. `strategy/match_read.rs` and `strategy/strategist.rs` port `MatchRead` and `Strategist`. The strategist diagnoses the match once a second and answers by match sense. With enough sense it learns over 30-second episodes which answer pays. The answer scales the weights and sets the stance: retreat health, the seek bar, shopping with armor, chasing, and fading. A fading bot runs to a graph node out of the enemy's sight (`movement/escape.rs`, `escapePoint`), then keeps to its route to health. `strategy/intel.rs` reads SHAG's `config/bot-intel`. With `people.json`, an unsure estimate of a human enemy leans toward the stack people typically have after so long alive. A map's `<map>.json` supplies what each answer paid in earlier matches as the strategist's starting estimates, once an answer has ten episodes. No map baseline is checked in yet. The intel store and the baseline export stay with the TypeScript tools. `movement/territory.rs` ports `BotTerritory`: in a Duel, holding high ground competes with the best item at `0.5 × mapControl`. A distant sound no longer pulls a bot off an item it needs, as in `StateMachineBrain` priority 2.5. `combat/lunge.rs` ports `SlideBladeSequence`. `BotPlayer::with_trace(true)` keeps a serializable `DecisionTrace` of each decision. Bots talk as the TypeScript bots do. `callouts.rs` ports `BotCallouts`: in team play a bot calls the busiest place with enemies in sight, the bomb carrier, and where the carried bomb was dropped or planted, by the map's `target_location` names. A teammate's call of enemies is somewhere to hunt for six seconds, weighed against the best item and the ground to hold. `chat.rs` ports `BotChat` and its lines: greetings, gg, and lines about humans it fragged or was fragged by, paced like typing and shaped by a personality from its name. Each directive's team call is said once. Lines go out as `Chat` messages through `NetworkedPlayer::chat_at`. `tools/bake-washoi-locations.mjs` bakes the location names into `<map>.locations.json`, which the supervisor passes as `locations_file`. `washoi_dm2` has none; `washoi_snd1` has 78.

Record why a candidate was gated, the scores that mattered, the winner, and the reason a running action ended. Use replayable observations and per-entity random seeds to reproduce decisions, including proficiency-related mistakes. Tune bot quality using scenarios such as pickup versus combat, losing sight of an opponent, continuing or aborting a traversal, and completing team objectives.

## Scheduling and performance

The TypeScript source already uses a shared timer, per-bot input pacing, and phase offsets for decisions. Keep those ideas. For Washoi, start with the existing 125 Hz input cadence (8 ms) and measure against it. The integration sets the cadence it requires. Tactical decisions can run less often and wake early for events such as a new threat or invalidated route. Movement and input generation can continue from the active plan between decisions.

### One scheduler per lobby

One logical scheduler owns the entity table and is the sole writer of live behavior state. The scheduler and protocol I/O can start on the same async executor if their work stays short. Entity types share the scheduler. Work per update, update frequency, and the number of active instances determine when more CPU capacity is useful.

The runtime accepts deadlines from the integration. In Washoi, all active bots start on the shared input cadence. Each bot also has a tactical decision deadline with a staggered phase. A relevant event can bring that decision forward. Keep the input and decision rates independently configurable inside the Washoi integration. Other integrations can set their own rates for each entity type.

1. Apply control events and ordered game updates. Process game traffic in bounded batches so a burst of ordinary updates cannot delay input or shutdown indefinitely.
2. Apply completed jobs that are still relevant. Advance due actions and emit inputs from the latest usable observation and active plan.
3. Run due tactical decisions within the update budget. Carry excess decisions forward in deadline order with a stable tie-breaker, so bots can progress even under load. A new plan can take effect on the next input tick.

Drive deadlines from a monotonic clock. If input wakes late, record how many deadlines passed and let the adapter follow its verified input sequencing and catch-up rules. Coalesce superseded snapshots only where the protocol allows it. Preserve ordered events and control messages. On stale game state or an invalidated plan, use the Washoi fallback input policy until fresh state or a valid plan arrives.

### Bounded CPU work

Implement the first Washoi slice with short inline decisions. Add a shared worker pool when profiling identifies a job that threatens input timing, such as route search, navigation building, or expensive tactical evaluation. Workers can take jobs from any entity type. If the per-entity work becomes costly enough to parallelize, submit one job or a batch of independent jobs through the same pool.

A job receives immutable inputs and returns a result to the scheduler. For example, a route job gets shared navigation data and a start, destination, and movement profile. Use read-only shared ownership for map geometry and navigation data. Deduplicate map loads and key cached navigation by map content and build inputs.

- For a per-entity job, carry an entity ID, spawn generation, map generation, and request ID. Apply the result only if it still matches the active request and its result is usable in the current state. A route can remain valid across many snapshots. Validate it against the current plan and route context.
- Keep one outstanding request per entity and job kind where possible. Supersede or cancel obsolete work, and bound queued jobs and results. When submission capacity runs out, retry eligible work on a later decision update and keep using the active plan if it is still valid.
- On shutdown, close job submission, cancel queued work, and signal running jobs. Long jobs check cancellation at bounded intervals. The host's shutdown deadline covers work that stalls. Worker count is a process-level budget and should be tuned with multiple simultaneous lobbies in mind.

For replays, use recorded observation and job-result events with a controllable clock and per-entity random seeds. Workers may finish at different times in live matches. Recording when results were applied lets us reproduce the decision inputs without relying on live thread ordering.

Compare the Rust and TypeScript runners on the same maps, scenarios, and bot counts. Collect CPU, memory, startup and map load times, tick and job latency percentiles, late inputs, queue depth, and stale results. Test with the Rust worker pool enabled and disabled once it exists. Set numeric load targets after recording the baseline.

## Washoi compatibility and migration

The TypeScript checkout has two hosting paths. The default lobby seats bots and starts a match runner per game server when needed. A separate `bot:server` can also receive authenticated requests from the lobby and use match runners. The match runner currently has an empty-process timeout. SHAGADELIC adopts the useful match-runner boundary with the lobby-bound lifetime defined above.

1. **Inventory the contracts.** Trace lobby admission and match tickets, connection negotiation, snapshots and input encoding, ratings, reports, and teardown. Check which protocol, collision, and asset-loading pieces already exist in headless Rust modules. Record gaps with representative fixtures.
2. **Prove the integration with one bot.** Build the child lifecycle and the first Washoi adapter, then admit a bot, receive snapshots, send valid paced input, and exit cleanly. Test failed join, removal during a join, an idle lobby, owner loss, and game-server termination.
3. **Establish feature parity.** Cover multiple bots, team and difficulty settings, movement and combat, navigation, items, and the objective behavior of Duel, Amphi, FFA, and S&D. Verify rating identity and match reports at the lobby boundary. Keep indexed PK3 map resolution and validate map, collision, and navigation results against the existing tooling.
4. **Improve behavior and throughput.** Expand skill ranking and tactical decisions behind recorded scenario comparisons. Evaluate CPU and input latency under load alongside behavior quality. Keep a TypeScript runner option available in the host during the comparison and rollout so each test lobby has exactly one bot implementation.

### First implementation sequence

**Implemented first milestone.** The Rust executable starts with a Node test host and keeps an empty lobby alive until shutdown or loss of its owner pipe. Control version 1 uses newline-delimited UTF-8 JSON with a 64 KiB input record limit, including the newline. The host sends `{"type":"initialize","version":1,"lobby_id":"test-lobby"}`. The child replies with `{"type":"ready","version":1,"lobby_id":"test-lobby"}`. The host sends `{"type":"shutdown"}` for a clean exit. Unsupported versions fail before readiness. Diagnostics go to stderr.

Run `cargo build` and `node tests/control.test.mjs ./target/debug/shagadelic.exe` on Windows. On Linux, use `./target/debug/shagadelic` as the executable path. The tests cover an idle lobby and explicit shutdown, an unsupported version, and abrupt host termination. They passed on Windows. Linux validation is pending.

**Implemented embedded runtime.** `Runtime<S>` accepts a `GameSession`, caller-supplied elapsed time, and separate event, input, and decision budgets. Active entities have independent deadlines. Generation keys isolate late admission and disconnect events from replacement entities. Tests drive two entity types at different rates with a paused simulation clock and exercise cancellation, duplicate spawns, decision wakeups, and shutdown. The first playable milestone admits one Washoi bot through both join stages and sends movement the game server accepts.

**Implemented Washoi WebSocket admission.** The optional `washoi` feature uses the headless `shagger_shared` codec from a pinned revision. The connection uses plain WebSocket for the local game-server endpoint, applies a deadline to connection and admission, and returns the same socket after the second accept. Fixtures cover both acceptance stages, the bot flag and ticket on Join, malformed frames, and rejection. A loopback WebSocket test covers successful admission and a server close between accepts. Run `cargo test --all-features` to include these tests.

**Implemented snapshot baseline tracking and ACKs.** The connection uses SHAGGER's headless decoder with a caller-supplied event codec. It ACKs accepted ticks on the socket, retains 128 baselines, and keeps one-tick events in the returned snapshot. Synthetic fixtures verify duplicates, malformed frames, missing and evicted baselines, and recovery on a full snapshot. The loopback test reconstructs a gameplay delta against a baseline received during admission. The SHAGGER pin matches the Rust Washoi client and includes the player appearance field. A regression fixture covers that field and the following event stream.

**Implemented typed movement submission.** The connection accepts a Washoi `PlayerInput` in world coordinates and attaches the playing identity, latest accepted snapshot tick, and incrementing command sequence. A loopback server decodes and verifies two movement frames. Tick pacing and action selection belong to the Washoi session and behavior layers.

**Implemented the Washoi event codec.** The decoder comes from `wash-fps-rust/src/shared/net/snapshot_events.rs` and uses the headless SHAGGER codec APIs. A snapshot fixture checks that a combat update and a match update survive decoding in wire order.

**Implemented pilot input pacing.** The first Washoi behavior walks forward while its own entity is present and alive. The standalone pilot uses an 8 ms timer, and the hosted Washoi session gets input opportunities from the shared scheduler. Both cap ordinary catch-up at four commands, discard debt above 25 ticks, and pause movement after 500 ms without a usable observation. A clock-controlled test covers those limits.

**Verified the first live bot.** Build with `cargo build --features washoi`. A test host can include `"pilot":{"endpoint":"ws://127.0.0.1:7777","name":"SHAGADELIC"}` in its version 1 initialization record. The pilot accepts optional `ticket` and `team` fields. It reports `pilot_admitted` after entering play and `pilot_ended` on disconnect or error. Host shutdown or owner pipe closure cancels the connection.

The live movement test passed on Windows against the TypeScript Washoi server on `washoi_dm2`. With a local standalone server running, set `SHAGADELIC_TEST_WASHOI` to its WebSocket endpoint and run `cargo test --features washoi --test washoi_live -- --ignored`. The test requires input acknowledgments and horizontal displacement in server snapshots. The standalone pilot also passed admission and explicit host shutdown against that server.

**Implemented the shared Washoi session.** `WashoiSession` implements `GameSession` and links each admission attempt and socket task to a runtime entity generation. The scheduler sends input opportunities through one-slot mailboxes. A slow socket won't accumulate an unbounded input queue. Socket tasks retain their own snapshot histories and complete each ACK before resuming their event loop.

**Implemented dynamic Washoi control.** Use `"washoi_endpoint":"ws://127.0.0.1:7777"` in the initialization record to start an empty session. A `spawn` record carries `id`, `name`, and optional `ticket`, `team`, and `difficulty`. The difficulty is a Washoi preset name: `easy`, `medium` (the default), `hard`, or `insane`. An unknown name fails the request with `unknown_difficulty`. The child reports `bot_status` with the generation and pending or active status, then `bot_admitted` with the playing ID or `bot_ended` with a reason. A `remove` record addresses an ID and generation. Invalid requests receive `bot_request_failed`. A repeated spawn for a live ID returns its current status. A replacement gets a new generation. The Node control suite exercised two simultaneous bots, removal, and replacement against the live server. Set `SHAGADELIC_TEST_WASHOI` to include this case when running the control suite with the Washoi-enabled binary.

The bot suppresses movement on death and waits 2.5 seconds before sending a respawn request. It retries at the same interval while fresh snapshots still show it as dead and resets pacing on confirmed respawn. A clock-controlled test covers the dead, stale, and revived states.

**Implemented authored waypoint navigation.** A Washoi session accepts a `navigation` object in its initialization record with a `map` and a list of `routes`. Each route contains an `entry` and ordered `waypoints` in Washoi world-space XYZ. The session shares the parsed data across bots and checks the map against the server's Welcome. A bot selects a nearby entry on its current floor, steers through its waypoints, and resets its route on death. It sends neutral input after arrival or when no same-floor route is available. Requests without navigation data use the forward-walk pilot.

The fixture at `tests/fixtures/washoi_dm2_navigation.json` defines short walking routes from the map's spawn points. The live waypoint test verifies arrival in server snapshots. Set `SHAGADELIC_TEST_WASHOI_NAVIGATION` to the fixture path to exercise shared route loading in the Node control suite.

**Implemented baked collision-aware walking.** The baker loads indexed PK3 geometry through Washoi's `HeadlessMapLoader`, checks floor support at both ends, and uses `BotVision.isWalkable` for each candidate edge. The Rust session accepts the resulting `navigation.graph` and derives shortest routes to reachable goals once at startup. Bots share these routes. The graph stores the BSP hash for provenance. The child checks the map name in Welcome, which carries no BSP hash. Rebake when the map package changes.

From the assembled SHAG checkout, run the baker under its existing `vite-node`. Supply absolute paths for the script, input, and output.

```text
node node_modules/vite-node/vite-node.mjs <SHAGADELIC>/tools/bake-washoi-walk-graph.mjs -- <authored-routes.json> <walk-graph.json>
```

The checked-in `tests/fixtures/washoi_dm2_walk_graph.json` captures a real blocked shortcut. A live Windows test forced the relevant spawn, killed the bot once, and verified arrival by the two-edge route after respawn. Set `SHAGADELIC_TEST_WASHOI_WALK_GRAPH` to run the waypoint test with a baked graph. This slice uses authored candidate nodes and static walkability. Dynamic blockers, ledges requiring maneuvers, off-route recovery, and the native BSP loader remain outside the current path.

### Reuse Washoi navigation assets

Baked graphs can carry a `maneuvers` list of directed `[from, to, action]` records alongside ordinary walk edges. The cache importer reads validated jump opportunities from SHAG's `src/games/fps/bot/BotNavGraph.ts` and `BotMovementAnalyzer.ts`, and authored trigger bounds and destinations from the same graph. The per-bot follower claims exact input through takeoff, launch or teleport and advances only after server observations confirm the landing or exit. Failed maneuvers enter the existing blocked-leg cooldown. The implementation follows `src/games/fps/bot/sequences/ValidatedJumpSequence.ts`, `TraverseJumppadSequence.ts`, and `TraverseTeleporterSequence.ts`. Objective routes still use walking edges.

The installed `washoi_dm2` cache bakes 27 jump links, four teleporters, and one jump pad alongside the walking graph. A live fixture verifies each maneuver type with authoritative snapshots. Run `node tests/washoi-traversal.test.mjs <assembled SHAG checkout> <baked DM2 graph> <jump|jumppad|teleporter>` from this checkout. Red Armor remains isolated in the installed cache. SHAG's jump analyzer rejected the nearby candidates tested on that BSP, so its recorded traversal remains a separate path to investigate.

The TypeScript runner reads its static graph from `.cache/bots/maps/<map>.json` or `BOT_NAV_CACHE_DIR`. SHAGADELIC's baker can import that existing cache through Washoi's `BotNavCacheStore`. The loader checks the cache schema, map name, BSP fingerprint, and movement versions against the indexed map. The importer extracts walk edges, with their `via` corners, and fall edges between spawn, floor, and item nodes. An edge is `[from, to]` or `[from, to, corners]`. Coordinates are rounded to a tenth of a unit. Large graphs load from a local file supplied in the initialization record. It uses spawns as entries and item positions as candidate destinations. The Rust planner weights edges by their length through the corners. Bots steer toward waypoints at other heights but count one reached only on its own level. Near a ledge before a drop, a grounded bot coasts to 100 u/s or less. Toward a lower waypoint, it leaves at the speed that lands on it under Washoi's gravity of 800 u/s².

```text
node node_modules/vite-node/vite-node.mjs <SHAGADELIC>/tools/bake-washoi-walk-graph.mjs -- --shag-cache washoi_dm2 <walk-graph.json>
```

This mode reused the existing `washoi_dm2` cache to generate 84 nodes and 329 edges in 12 KB. The largest walkable area holds 44 nodes, 14 of the 18 goals, and five of the six spawns. The `washoi_snd1`, `washoi_snd4`, and `washoi_snd6` caches yield 302, 480, and 649 walk nodes. The larger S&D graphs exceed a single control record, so the supervisor passes the baked path as `navigation_file`. The child loads it once and bounds it to 4 MiB, 1,024 nodes, 64 entry points, and 32,768 edges. Graph searches use a priority queue with deterministic ties. Set `SHAGADELIC_TEST_WASHOI_ROAM_GRAPH` to a cache-mode graph to run the ignored `washoi_bot_roams_between_graph_goals` test. It requires three distinct goals in one life within 25 seconds, and it passed ten consecutive Windows runs on `washoi_dm2`.

Some SHAG cache spawns sit at floor height, which can classify nearby links as zero-speed jumps. The baker tests those links at standing hull height through SHAG's walk checker and exports the ones that pass as walks. This restored connectivity at the `washoi_snd1` attack spawns. In a live four-bot S&D match, bots collected the bomb, followed an eight-waypoint staging lane, and triggered the server's transition to `post_plant`. Defenders have reached the planted bomb in live retakes, but the fuse expired before they completed the defuse. To exercise the plant, bake `washoi_snd1.json` into a navigation directory and run `node tests/washoi-snd.test.mjs <washoi-enabled executable> <assembled SHAG checkout> <navigation directory>`.

The cache importer also preserves stable pickup IDs and item categories from the authoritative map table. It adds pickup nodes as possible re-entry points within the graph's entry limit. Among pickup paths from the chosen entry, the bot scores health and armor need, pickup type, and walking distance. The bot consults availability only after reaching the item's waypoint and expects a missing item back after its respawn time from Washoi's `itemRespawnSeconds`. That is the full time when the bot saw the item vanish from close by, and half of it when the bot found the spot already empty. A spot found empty is also off by up to `max(0.5, 7.83 - 8.33 * itemTimingSkill)` seconds either way, drawn uniformly, as in `BotPerception._believeItems`. That is 7 seconds for easy and 0.5 for insane. The bot keeps that memory, along with recently blocked route legs, through death and learned traversals. It can then choose another reachable pickup. The bot skips health and armor that Washoi's `PickupSystem` would leave in place: health at 100, or 200 for the overhealing small, mega, and ultra kinds, and armor at 100. The baker records each pickup's item kind as `item` for this check. Without it, health is treated as capped at 100. If an item is still there 400 ms after arrival, as with full ammo or a new weapon that needs USE to swap, the bot leaves it alone for 15 seconds. A live `washoi_dm2` fixture damaged the bot and verified that it recovered health when the server marked pickup 5 inactive. Pickup timing, weapon ownership checks, and goal choice between combat and resource collection remain open.

Weapons and ammo are expected back after 15 seconds, the `itemRespawnSeconds` default that the TypeScript bot also assumes. Washoi's gametypes currently override weapons to 30 seconds (20 in S&D). The override is a Washoi bug that a Washoi release will fix, so the bot keeps 15 seconds.

**Implemented line of sight.** TypeScript `BotVision.canSee` traces a point ray with the player content mask, SOLID and PLAYERCLIP, through the map's collision. `tools/bake-washoi-sight.mjs` exports the static part of that collision from the indexed BSP: world brushes and clip brushes as plane sets, and the tessellated collision triangles of curved patches, in game space. Doors, lifts, and other switchable brush models are left out, so they never block sight. The binary format starts with `SHAGSGT1` and a JSON header with the map name and BSP fingerprint. `Sight` loads it once per session, builds a bounding volume tree, clips brushes as `SweepCollisionBackend.traceBrush` does, and tests patch triangles directly. On `washoi_dm2`, the file holds 2,994 brushes, 23,300 planes, and 8,400 triangles in 687 KB.

```text
node node_modules/vite-node/vite-node.mjs <SHAGADELIC>/tools/bake-washoi-sight.mjs -- washoi_dm2 <washoi_dm2.sight> --samples <sight-lines.json>
```

`--samples` also writes eye- and knee-height sight lines between spawns and pickups, answered by SHAG's own trace. All 1,128 `washoi_dm2` lines agreed with the Rust tracer. On `washoi_snd1`, 14 of 10,011 lines differed in both the original and allocation-free traversals. The Rust static trace reported those lines clear. Set `SHAGADELIC_TEST_WASHOI_SIGHT` and `SHAGADELIC_TEST_WASHOI_SIGHT_LINES` to run the ignored `agrees_with_shag_on_baked_sight_lines` test. The control record names the file as `sight_file`. All map assets in one initialization must name the same map, and each bot ends with `navigation_map_mismatch` when the server reports another map. TypeScript's PVS precheck only skips traces, so the Rust tracer omits it.

**Implemented a first fight layer.** With a sight file, a bot fights the nearest enemy it has noticed. Sight runs from 32 units above its origin to 25.6 units above the enemy's. Allies on a team, spectators, and players the server reports dead are skipped. `weapons.rs` holds the profiles that `BotWeaponProfile.getWeaponProfile` builds from Washoi's weapon definitions and ports `selectBestWeapon`, so a bot that spawns holding the blade switches to a gun for the range. Ammo is clip plus reserve, as `normalizeBotAmmo` sums it. Aim follows `states/BotFight.ts`. Shots leave from the 34-unit view height. Projectiles lead by their speed, and splash weapons aim 20 units low. Other weapons lead by `predictionSkill` at an assumed 1,400 u/s. The bot holds alt-fire to aim down sights once the chosen weapon is out. It holds fire until the weapon's `adsReadyTicks` have passed. A blade swings only within 150 units, and fire alternates between presses, as in `pulseMeleeFire`. Movement keeps following the walk graph while the bot fights.

`aim.rs` ports `BotAimController` and runs once per command. The view follows a critically damped spring, capped at the preset's top speed. A turn over 45° waits a log-normal reaction time and sometimes overshoots. A wandering offset supplies aim error, and pursuit tracks a target under fire. Out of a fight, the view eases toward the walking direction. Recorded traversals and takeoff approaches set the view themselves. Each bot draws from its own seeded SplitMix64 source.

The ignored `washoi_bots_fight_until_one_kills_the_other` test runs two Insane bots with the cache graph and a sight file. It needs `SHAGADELIC_TEST_WASHOI`, `SHAGADELIC_TEST_WASHOI_ROAM_GRAPH`, and `SHAGADELIC_TEST_WASHOI_SIGHT`, and passes when one kills the other within 120 seconds. Before weapon choice, the bots held the blade and rarely hit each other. With it, a hit came within 5 to 18 seconds in five of five runs, but kills came in only three of five. In the other two, both bots stood still out of each other's sight, about 50 units from a graph node and past the 48-unit planning radius. The last waypoint each had reached was a route corner. Recovery accepted only a node, so the bots never replanned. A bot that last reached a corner now walks back to it and finishes that walk to its end node. After that fix, ten of ten runs reached a kill within 7 to 23 seconds.

**Implemented fight movement.** `strafe.rs` ports the strafe rhythm and hit dodges of `BotCombatMovement.ts`. Runs last 0.25 to 0.9 seconds by `dodgeSkill`, and skilled bots sometimes stop or double back. A hit sets off a 0.3-second jump dodge, or from 200 u/s a 0.5-second crouch slide. Strafing is relative to the wanted view and adds to the walk direction, and the virtual keys snap the sum to a chord. TypeScript keeps the feet clear with collision sweeps in `BotVisionTracer.ts`. Here a strafe side needs a clear sight ray 48 units to that side and floor within 40 units below the feet there, and the other side is tried when it lacks either. Slow progress while fighting doesn't mark a leg as blocked. Recovery then walks back to the last waypoint, or straight to one of the four nearest nodes within 128 units that the `local_walk` check (body-width clearance and floor along the line) finds clear. On a separate server, two strafing Insane bots reached a kill in 22 of 22 runs, within 10 to 29 seconds, against 7 to 35 seconds without strafing. Runs on a server that another session's bots shared failed more often, so live tests need their own port. A rocket or grenade whose path passes within 150 units in the next second is sidestepped once the reaction time has passed, with probability `dodgeSkill`, as `_reactToRocket` does. One the bot first sees within 120 units is its own. TypeScript reads every `proj_` entity in the snapshot, but here a projectile counts only when a sight ray from the eye reaches it. With rocket sidesteps, 22 of 23 runs reached a kill. High ground and cover are not ported yet.

**Implemented local steering, without ground turns.** `navigation/clearance.rs` ports `BotSteering.ts` and `BotVisionTracer.ts` over the shared collision world and the BSP trigger table. In the air, a bot about to fly into a wall presses the side key toward the open side at the angle that turns the flight without losing speed (`steerAir`). A bot thrown by a jump pad that no traversal flies rides on to the pad's destination; pads whose destination is less than 64 units to the side of them are skipped, and a pad that threw the bot back onto itself rests 250 ticks. `navigation/tricks.rs` ports `tryTricks`: on a route walk, a jump onto an obstacle when the walk's point is above it, and a wall jump when one ends nearer the point, each proven first by running Washoi's movement forward with the keys a person would hold. Out of a fight the view turns with the feet, so the keys stay those of a player. The ground probe, which turns the walk onto the nearest clear heading as TypeScript's feet do, runs but turns nothing (`STEER_GROUND`). In Duels against the TypeScript bots on `washoi_dm2`, every variant that turned the walk lost frags and run speed to main, and the build without ground turns was no worse (benches 3 to 5 in WORK_SPLIT.md).

**Implemented perception of enemies.** `perception.rs` ports `BotPerception._sortPlayers` and `_hear`, which the TypeScript client runs by default. An earlier version of this design read `BotVision` alone and let bots see all around. The eight nearest enemies are checked each snapshot. An enemy is checked when it is within 55° of the bot's current view, within 70° while running faster than 250 u/s, seen in the last second, or heard this snapshot. A checked enemy counts as seen once it has stayed in line of sight for half the reaction time. That is 18 ticks for medium and 4 for insane. The bot hears shots within 1,200 units, or 2,000 for rockets, grenades, electrobolts, and electro rails. It hears pain within 800, pickups within 900, footsteps within 500, and jumps within 720. A player within 200 units of an explosion that lands within 1,500 of the bot is heard too, and an attacker is revealed by its hit wherever it is. Major item pickups, which carry 2,500 units in TypeScript, use the ordinary range. With the view cone, nine of ten live fights reached a kill within 120 seconds, taking 12 to 42 seconds. Bots have no hunt behavior yet and meet while roaming for pickups.

**Implemented chasing an enemy out of sight.** The bot remembers the last enemy it fought, as a `BotVision.ts` visual track does. Confidence falls from 1 to 0 over four seconds, and the position is extrapolated from the last seen velocity for up to three seconds. While confidence is at least 0.3, a bot with no enemy in sight and no server order walks toward the predicted spot. The walk is an objective walk on the graph, replanned when the prediction moves 96 units. Like `BotRoam.chase`, it watches that spot and fires into it within 200 units while confidence is above 0.7, except with the blade. A death or a faded track ends the chase. Heard enemies and attackers also lead a chase, starting at 0.6 and 0.8 confidence and fading in 2.5 seconds, unless a surer track of another enemy is fresh. TypeScript keeps several tracks at once and prefers seen ones. Ten of ten live fights reached a kill within 12 to 32 seconds, and the roaming test still passes.

**Implemented weapon combos.** `combos.rs` ports `BotWeaponCombos.ts`. After the bot's own rocket hit, it switches to the blade within 150 units, or, while the enemy rises faster than 150 u/s, to the electrobolt from 400 units or the riotgun closer. Plasma held for 1.5 seconds beyond 500 units is broken by a 0.7-second electrobolt shot. A slow gun just fired is swapped for the carried gun with the most damage per second that fits the distance and can fire at least 0.25 seconds sooner, held until the slow gun is ready. Weapon profiles carry TypeScript's fire intervals for this. Each chance is rolled once at `predictionSkill` odds from a per-bot random source. The finishing switch, to the hitscan gun whose one hit kills, needs `MatchRead`'s estimate of enemy health and is not ported. Hosted live fights reached a kill in five of five runs within 16 to 29 seconds.

**Implemented cover peeking.** `cover.rs` ports `BotCoverPeek.ts`. A ranged fight beyond 350 units, with the bot on the ground and the chosen weapon out, rolls at `dodgeSkill` every half second to look for cover. It looks 64, 112, 160, and 208 units to either side, square to the enemy, for the nearest spot where the sight trace can't reach the enemy's eye and `local_walk` finds a straight walk. With a weapon of half a second or more between shots, the bot steps into cover after each shot and back out just before the next. With a faster weapon it ducks for 0.35 to 0.5 seconds between 0.6- to 1-second bursts. While peeking, the side step replaces the walk direction. Ducked out of sight, the fight goes on without fire, watching the enemy's last seen spot, for at least a second or a reload and a step. Then the chase takes over. TypeScript holds only while the last peek was two ticks ago, because it decides every tick. The Rust brain decides per snapshot, so it allows 16 ticks. With cover, direct live fights reached a kill in 18 of 18 runs and hosted ones in 12 of 14.

**Implemented high ground.** `highground.rs` ports `findHighGround` and `BotFight.highGround`. Every two seconds of a fight within 900 units with a splash weapon, out of cover, the bot rolls at `dodgeSkill`. It then looks for the highest graph node within 300 units whose floor is at least 64 units above both its origin and the enemy's and within 900 of the enemy. The enemy must be visible from 40 units above that floor. Baked nodes are standing origins, 24 units above TypeScript's floor points, and the checks convert. Unlike TypeScript, which turns the move toward the spot and relies on its tracer, the bot walks there on the graph and skips nodes the objective planner can't reach. The walk replaces the strafe until the bot is within 48 units. A server order outranks both the climb and a cover peek. While one steers, fight movement only adds the strafe and dodges to the order's walk. Before that rule, the climb took an S&D attacker away from the plant site, and the live four-bot plant test on `washoi_snd1` failed. A bisect by session B named this commit (`a189c5f`). On the `washoi_dm2` graph, 96 of 644 mutually visible node pairs within 900 units offer such a ledge. `keepUpperGround` keeps a grounded fighting bot that stands 64 units or more above the enemy on its ledge. Its move stands when 96 units along it are walkable by `local_walk` with floor within 64 below. Otherwise it takes the first of a quarter turn either way or a reversal that is. With high ground, direct live fights reached a kill in six of six runs and hosted ones in three of three.

**Implemented charged blade blasts.** Following `BotFight.updateChargedFire`, a bot holding the blade beyond its 150-unit swing reach holds alt-fire to charge a blast. A charge starts only when the bot means to fire. It is held for the 63-tick minimum plus a share of the 62 ticks up to the maximum, set by `0.35 + 0.4 * predictionSkill` with ±0.075 jitter. It is let go once the sent view is within 12° of the aim, or after a half-second grace. The blade's profile now carries its charge times and the blast's 2,500 u/s, so the aim leads like a projectile's. Out of a fight, `carryCharge` keeps holding for up to two seconds while the blade stays out, so the bot can let go when the enemy is back. Direct live fights reached a kill in five of five runs and hosted ones in three of three. In 10 runs each on session B's server, hosted fights reached a kill in 9 of 10 before cover and 10 of 10 after. Fights got somewhat longer with cover.

**Implemented an enemy health estimate and the finishing switch.** `estimate.rs` keeps the part of `strategy/MatchRead.ts` that fights use. An enemy first seen alive, or seen again after a death, has 100 health and no armor. Each hit the bot lands loses armor's 66% share while the armor lasts, and the hit's pain tier clamps health to 1 to 20, 21 to 35, 36 to 60, or 61 to 200. Uncertainty grows to 1 over 15 seconds without news. The combo layer's `_finish` port picks, at an uncertainty of 0.3 or less, the carried hitscan gun with the smallest hit that still kills through the armor. Half a riotgun's pellets count as landing, and guns more than 25% past their ideal range are excluded. It is rolled once per change in the estimate at `predictionSkill` odds and held for 0.8 seconds. TypeScript also credits major items an enemy was seen or heard taking, which needs pickup hearing by item and is not ported. So the estimate can run low after an unseen Mega Health. Direct live fights reached a kill in five of five runs and hosted ones in three of three.

**Implemented readying a gun out of a fight.** Following `BotFight.readyForHeard`, a bot that isn't fighting gets out the gun for the distance of the nearest enemy heard but not seen within 1,500 units. This is rolled every two seconds at `predictionSkill` odds and kept for two seconds. Otherwise it puts the blade away for the gun a fight at 800 units would pick, unless a blade charge is being carried. It also runs while a server order steers, and changes only the weapon slot. Direct live fights reached a kill in five of five runs and hosted ones in three of three.

**Measuring parity against the TypeScript bot.** `tools/bench-washoi-ab.mjs` plays a Rust bot against a TypeScript bot over many headless Duels, as SHAG's `bot:ab` plays two TypeScript arms. Each match gets an in-process `GameServer` at 125 Hz with 60 Hz snapshots and 15 s pickup respawns. The TypeScript bot joins through `BotServer`, the Rust bot through the supervisor with a navigation directory holding `<map>.json` and `<map>.sight`, and join order alternates. It reports wins, a sign test, the per-match frag gap with a 95% interval, and total frags and deaths. Run it from the SHAG checkout:

```text
node node_modules/vite-node/vite-node.mjs <SHAGADELIC>/tools/bench-washoi-ab.mjs -- --exe <shagadelic.exe> --nav <navigation dir> --matches 12 --seconds 120 --difficulty hard
```

Run it from a SHAG checkout pinned to a commit, with the `src/games/fps` submodule pinned too. Results from a working tree with uncommitted bot or protocol edits are not comparable. On 2026-11-22, a checkout with local `BotVision.ts` edits made main look ahead of the TypeScript bot.

Results on `washoi_dm2`, Duel, hard, release builds, against SHAG `f37c56ec` with the game at `fc9b43d`:

| Rust build | Matches | Wins, Rust–TS–draws | Frag gap per match (95%) | Frags | Deaths |
| --- | --- | --- | --- | --- | --- |
| `9450cae`, before mood, look, and fire safety (debug build, unpinned SHAG) | 12 | 1–7–4 | −1.00 ± 0.99 | 17–29 | 31–19 |
| `854edcf`, main before the goal arbiter | 16 | 3–12–1 | −1.44 ± 0.96 | 19–42 | 48–25 |
| `7916d3d`, the goal arbiter and strategy layer | 16 | 7–8–1 | −0.56 ± 1.15 | 32–41 | 48–39 |

The arbiter's frag gap improves on main's by 0.88 per match. A session B trace of the arbiter build found no bot standing still for 5 s or more. In that run the Rust deaths exceeded the opponent's frags by about seven per 16 matches. Session B's traced Duels after the item-picker fix (`ebe4614`), which classify each death, found one self-kill in 16 matches, so the excess is unconfirmed.

**Hunting is mostly covered.** TypeScript `BotRoam.hunt` goes after the freshest enemy sighting within 300 server ticks (2.4 seconds). Otherwise it takes an item goal or patrols a random graph node. With `scoredGoals`, it weighs those by desires from the bot's weight profile. The Rust chase already follows seen and heard enemies for up to 2.8 seconds, and roaming walks between pickup and plain goals. The weighted choice between hunting a teammate's call, items, and territory is now `BotPlayer::hold_ground`, scored with the strategy layer's weight profiles.

The demo workflow is a separate source. `training_demo` recordings processed by `bot:learn` produce `config/bot-routes/<map>.json`. Those records carry a start pose, landing, and run-length-encoded commands for deterministic maneuvers. `config/bot-lanes/<map>.json`, written by `bot:lanes`, can feed S&D tactics when that layer is implemented.

**Implemented learned-route playback.** A Washoi session can include a `learned_routes_file` path in its initialization record. It loads the original version 1 Washoi file once and shares it across bots. A bot can attempt a recording when it observes itself standing still within the recorded takeoff tolerance. It plays the exact command sequence at the logical input cadence, keeps the order across short stalls, and checks the landing against the server's position and grounded state. Stale snapshots or excessive input backlog abort playback.

An authored walking route can name a `learned_route` ID. Its last waypoint must match the recording's takeoff tolerance. The baker preserves the link on the graph goal. Once the bot reaches the final walking leg near that goal, it crouches toward the mark, coasts to manage speed, and waits until it is standing and nearly still before playing the named recording.

**Implemented automatic learned links and landing continuations.** Add `--learned <bot-routes.json>` to either baker mode. The baker accepts recordings with a collision-checked walk from an entry-reachable node to takeoff and a walk from landing to a node that can reach an ordinary goal. The takeoff walks back to each approach node that the same check passes from it, as TypeScript's `addLearnedRoutes` jumps from an ordinary standing node. As a node with no walk out, a takeoff beside a walkway was a planning start whose only way on was its recording: on washoi_dm2, bots crossed between the upper walkway and the Red Armor for minutes. It links up to 16 recordings. The planner keeps a learned option alongside ordinary paths and prefers it when it can attach at the same entry distance. After a verified landing, the bot can take a walking continuation associated with that recording. A failed playback can fall back to ordinary walking. The planner links each baked takeoff to its landing for the recordings the session loaded, at the recording's ticks as distance at 320 u/s, as `BotNavGraph.addLearnedRoutes` adds learned edges, so goals past a recording, such as washoi_dm2's Red Armor, can be chosen and walked to. A life may take another recording once a landed one's walk on is under way. A failed one keeps the bot off its takeoff for the follower's five-second retry delay, as a failed traversal blocks its leg and TypeScript cools a failed step down. While a bot can't take off, goal choice and planning both use the walk graph alone, the links masked, so no goal past a takeoff is chosen and then refused. A pocket keeps the links: a node whose walkable goals all walk to no other goal, as on the Red Armor ledge, whose bake has no walk down. Without them, a bot there had no plan and stood still for the match.

The live fixture verified `wdm2_ra_jump@6146` from a spawn 48 units away on `washoi_dm2`, with spawn jitter disabled and the approach and exit baked against the indexed map. It confirmed another walking goal 72 units past the recorded landing. For that fixture, run the ignored `washoi_replays_the_existing_learned_route` test with `SHAGADELIC_TEST_WASHOI_LEARNED`, `SHAGADELIC_TEST_WASHOI_APPROACH`, `SHAGADELIC_TEST_WASHOI_WALK_GRAPH`, and `SHAGADELIC_TEST_WASHOI_CONTINUE`.

The Windows live test also passed with an indexed `washoi_dm2` collision world and a fixture that killed the joined bot once through Washoi's combat system. The server logged the bot's `RequestSpawn`, and the test observed the dead-to-alive transition and subsequent waypoint arrival. To require that sequence, set `SHAGADELIC_TEST_WASHOI_RESPAWN` and run the ignored `washoi_bot_reaches_an_authored_waypoint` test against a local server that kills the bot once.

The positional order fixture exercises Washoi's snapshot encoding and movement against the indexed `washoi_dm2` collision world. It verifies that the bot receives its order, passes the assigned corner, and holds the destination for 40 server ticks. From the SHAG checkout, set `SHAGADELIC_TEST_PORT` to a free local port and run `node node_modules/vite-node/vite-node.mjs <SHAGADELIC>/tests/fixtures/washoi-orders-server.ts`. From SHAGADELIC, set `SHAGADELIC_TEST_WASHOI` to that port's WebSocket endpoint and run `cargo test --features washoi --test washoi_orders -- --ignored`.

The live movement test also passed against a server configured with a match admission manifest containing a bot seat. Set `SHAGADELIC_TEST_WASHOI_TICKET` for that test when the local server requires a ticket. The test hands it to the join encoder without printing it.

**Implemented the Node match supervisor.** `tools/washoi-supervisor.mjs` starts one child per match ID, waits for the versioned readiness record, and forwards existing seat tickets and difficulty preset names in spawn records. Custom difficulty objects are rejected before they reach the child, because a malformed control record ends the child. `start(matchId, endpoint, map)` can attach per-map assets. With `SHAGADELIC_NAVIGATION_DIR` set, it passes `<map>.json` as `navigation_file` and `<map>.sight` as `sight_file`. With `SHAGADELIC_LEARNED_ROUTES_DIR` set, it passes `<map>.json` as `learned_routes_file`. The child checks the asset map names against the supervisor's `washoi_map` before readiness. Direct control clients can still send inline `navigation`. A missing file starts the match without that asset. The supervisor correlates removal with the reported entity generation, gives the child two seconds to shut down before terminating it, and waits for process and stream closure. The tests cover empty matches, two match IDs sharing an endpoint, and disposal and shutdown with live bots. Run `node tests/washoi-supervisor.test.mjs ./target/debug/shagadelic.exe` with the Washoi-enabled binary. Set `SHAGADELIC_TEST_WASHOI` for the live test.

**Integrated the Washoi lobby on the `shagadelic-lobby` branches.** SHAG commit `21998d5c` invokes the runner on match-server readiness and stops it on game-server exit. It references Washoi commit `ae0996a`, which adapts lobby bot seats to the Node supervisor. Start the lobby with `SHAGADELIC_EXECUTABLE` set to the built binary and `SHAGADELIC_SUPERVISOR` set to the absolute path of `tools/washoi-supervisor.mjs`. The experimental runner connects to the local plain WebSocket game port. Lobby-managed matches containing Rust bots are unrated while behavior and reporting are under development. The TypeScript runner is the default.

The bundled lobby passed a Windows smoke test with a private Duel room on `washoi_dm2`. A bot invite led to match startup, child readiness, and admission with the seat ticket. The lifecycle test verifies the game-server exit callback for a match with zero bots. The live Node supervisor test verifies child reaping while a bot is still connected.

The lobby runner forwards the spawned server's map and the invited bot's named difficulty preset. The supervisor loads configured map assets before readiness, and Rust applies the existing default when the caller supplies an unsupported difficulty or a custom profile. A private Duel smoke test voted for `washoi_dm2`, loaded its baked graph, and admitted a ticketed Hard bot. The forwarding and fallback are covered by `src/games/fps/bot/__tests__/ShagadelicRunnerPool.test.ts` on the Washoi integration branch at `1ee9da7`, referenced by SHAG integration commit `b99e0d25`.

| Change | Implementation | Acceptance |
| --- | --- | --- |
| 1. Host contract and Rust entry point | Trace the current Node runner launch, message ordering, and match teardown. Add the Cargo package and small `control` module with startup, readiness, shutdown, and diagnostics. Use a fake host to exercise the chosen framing. | The child reaches ready, stays alive in an empty lobby, and exits on shutdown or loss of the owner channel. Verify process cleanup on Windows and Linux. |
| 2. Entity lifecycle and scheduler | Add a library target with an entity table, generation tracking, cancellation, and a caller-driven scheduler. Exercise it through an embedded fake game with different input and decision rates and use a fixture adapter for asynchronous admission. | Duplicate spawn, remove during admission, and late admission after cancellation behave predictably. The embedded test advances simulation ticks explicitly, including a pause, and verifies separate rates for two entity types. |
| 3. First Washoi client | Implement the Washoi adapter and a minimal `BotPlayer`. Reuse compatible codec pieces after checking join, snapshot acknowledgment, and input-encoding fixtures. Wire the child into the Washoi host behind an explicit runner selection. | A real Washoi server admits one bot using the host's ticket, accepts its paced inputs, and the child exits when the lobby or game server ends. A replay verifies that an interrupted input sequence stops cleanly. |
| 4. Several Washoi bots | Add per-bot observation and memory, shared map data, phased decision updates, and the admission and reporting events needed by the host. Capture a TypeScript baseline under representative load. | Several bots keep separate client identities and state, share map data, and remain independently removable. The host can compare input timing and resource use with the current runner. |
| 5. Skills and jobs | Port feature groups in small slices. Introduce goal and skill scoring traces and add the first worker job when profiling identifies it. Route search is a candidate. | Replay checks cover skill gates, commitment and interruption, and stale results. A saturation test shows that input pacing and shutdown remain responsive while workers are busy. |

## Repository conventions

Use a Cargo-first repository with a `shagadelic` library and headless executable in one package. Use the neighboring Rust repositories' stable toolchain convention with `rustfmt` and `clippy`, Rust 2024 edition, snake_case module names, `tests/` for integration and cross-language fixtures, and `tools/` for development scripts when needed. Research and design notes live in `docs/research/`. Add `docs/manual/` and the familiar `docs:dev`, `docs:build`, and `docs:preview` scripts if the project gets a browsable manual.

```text
docs/research/SHAGADELIC.md
src/
  lib.rs                   Embeddable session runtime and scheduler
  main.rs                  Standalone Washoi host
  control/                 Host IPC, framing, and process lifecycle
  runtime/
    mod.rs                 Lobby session, lifecycle events, and entity keys
    scheduler.rs           Input and decision deadlines
  games/
    mod.rs                 Integration selection and protocol adapter contract
    washoi/
      mod.rs
      session.rs           Entity routing, input pacing, and shared map services
      protocol/
        mod.rs             Washoi adapter and typed protocol events
        connection.rs      WebSocket handshake and per-client wire state
      world/
        mod.rs             Decoded game state and per-player observations
      behavior/
        mod.rs
        player.rs          BotPlayer, perception memory, and plan execution
        input.rs           Typed Washoi PlayerInput
tests/
  fixtures/                Washoi wire fixtures and replay inputs
tools/
```

Use standard Cargo entry points for build, test, lint, and formatting. Keep any launch wrappers thin and match existing SHAG/Washoi command terminology where it helps operators. Prefer an existing compatible headless API from SHAGGER or the Washoi Rust client for protocol codecs, math, collision, and asset handling. Verify wire compatibility with fixtures and examine the dependency and feature boundaries before choosing a crate. Keep game-specific dependencies inside the Washoi integration where practical, and introduce a Cargo workspace when there is a concrete need for separately reusable crates.

## Open decisions

### Implementation risks identified in review

- **Lobby lifetime.** The current pool starts a runner on the first bot request and indexes it by server URL. Selecting a different executable in that path alone would leave an empty lobby without a child. Add explicit session-start and session-end hooks to the host and identify each session incarnation independently of its endpoint. Test two consecutive games reusing a port, with a late exit notification from the first child.
- **Control transport.** Launch the Rust executable directly through Node's `spawn`. Frame UTF-8 JSON records with newlines, a maximum record length, and incremental decoding for partial reads. Have the host continuously consume stdout and stderr. On clean shutdown, consume final report records through stream closure. Node distinguishes process exit from stdio closure, and an undrained pipe can block a child. Test a final report split across reads during exit. [Node child-process documentation](https://nodejs.org/api/child_process.html)
- **Input pacing.** The TypeScript client has separate ordinary and scripted catch-up paths. Both cap a burst at four commands. Ordinary pacing discards accumulated debt beyond 25 ticks, while a script advances through its ordered inputs. Use a fake clock with uneven and stalled wakeups and compare command sequences against both paths. Measure scheduled-to-actual send delay and longest input gap per bot alongside scheduler duration. A fast scheduler can still produce late input at a congested socket.
- **Worker shutdown.** If Tokio is selected, acquire a bounded CPU permit before submitting blocking work. A running blocking task continues after its handle is aborted. Check a cancellation flag between bounded units of route expansion, and test shutdown while a real search is running. [Tokio blocking-task documentation](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html)
- **Intelligence evaluation.** Separate decision quality from faster execution in the benchmark. Feed both implementations the same observations, initial profile and scenario objective. Record goal switches, progress toward the objective, time stuck, and skill interruptions. For live matches, swap sides and spawn positions across repeated trials. Use paired scenario outcomes alongside timing percentiles to choose which changes to retain.

### Questions for the first implementation

- For lobby-managed game servers, bind child supervision to the successful game-server launch and extend the existing `LobbyServer` handler for `GameServerSpawner.onServerExit(matchId, code)`. This supplies an authoritative end signal even with zero connected bots. Verify which additional hosting paths bypass this spawner and need an equivalent owner.
- Which current runner fields and report ordering must carry over exactly, and how will the host select the Rust runner during rollout?
- Which SHAGGER APIs can be consumed headlessly today, and which need a narrower interface for server use?
- Which maps, game modes, and bot counts should define performance acceptance, and which recorded scenarios should define improved behavior?

## Source references

Paths below are relative to the corresponding sibling repository checkout.

- SHAG/Washoi lifecycle and existing contracts — `src/games/fps/docs/manual/tools/bot-server.md`, `src/games/fps/cli/lobby-server.ts`, `src/games/fps/cli/bot-runner.ts`, and `src/games/fps/bot/runner/MatchRunnerProtocol.ts`.
- Washoi client, scheduling, and decisions — `src/games/fps/bot/BotClient.ts`, `src/games/fps/bot/BotBrain.ts`, `src/games/fps/bot/BotScheduler.ts`, `src/games/fps/bot/BotConfig.ts`, `src/games/fps/bot/goals/GoalArbiter.ts`, and `src/games/fps/bot/InputClaims.ts`. The manual describes per-bot timers. The scheduler source implements one timer for the process.
- Rust conventions and reuse candidates — `wash-fps-rust/Cargo.toml` and `rust-toolchain.toml`, plus SHAGGER's `crates/shagger_shared/src/net/binary_protocol.rs`, `crates/shagger_shared/tests/protocol_golden.rs`, and `crates/shagger/src/loader/pk3/`.
