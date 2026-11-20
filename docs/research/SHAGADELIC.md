# SHAGADELIC

Draft architecture for a Rust service. The first integration is Washoi AI players.

SHAGADELIC runs the AI entities for one game session. A host can run it as a supervised child process or embed its Rust library in the game. Washoi is the first integration and uses the child process. Each Washoi bot joins as a headless client and plays through the same input protocol as a human client. The existing TypeScript bots establish the compatibility and feature baseline. Their behavior and code are candidates for reuse where they help reach that baseline.

The first release aims for predictable runtime cost and bots that make better decisions. Future integrations may run several entity types in the same lobby, such as soldiers and imps, or entities with entirely different controls, such as drones and fish. The first release implements Washoi only.

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

Use the game-session boundary for both forms of hosting. Washoi implements network admission and wire transport through `GameProtocol`. An embedded integration registers entities against the local simulation and exchanges observations and actions by direct calls. Keep game-specific types in the integration and let the first real embedded game inform the public API.

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

Keep these three inputs to decision-making distinct.

| Concept | Washoi examples |
| --- | --- |
| Capability | The entity can track a target, dodge, or execute a crouch-slide sequence. |
| Proficiency | A profile sets reaction delay, tracking accuracy, movement reliability, item timing, and tactical awareness. Existing difficulty presets map to these values. |
| Decision policy | The bot evaluates whether to contest a pickup, hold position, disengage, or choose another goal. It picks skills that can execute the chosen goal under current conditions. |

The first Washoi behavior should preserve the useful parts of the TypeScript goal arbiter and input claims. Give each candidate a stable ID, eligibility gates, and a score with named factors. Reject inapplicable actions before scoring. Rank the eligible goals by their current value, with stable ties and a commitment window to limit indecision. For the selected goal, rank the applicable skills by expected effectiveness, risk, execution cost, and the bot's proficiency. A skill declares its prerequisites, input channels, and completion or interruption conditions. Rank only candidates the bot can actually use. A full sorted list is useful in a decision trace, while the hot path can select the best candidate in one pass when it needs only a winner.

A maneuver can run over multiple ticks. It retains its action channel claims until completion or an allowed interruption. For Washoi, independent view, movement, and button channels let a bot track an opponent while finishing a movement sequence. Define what can preempt a claim, what happens when a target or route becomes invalid, and what fallback action the entity takes. Keep Washoi-specific skill data and policies in the Washoi integration. Reuse a selection or execution primitive in the runtime once a second behavior provides evidence that the primitive is shared.

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

**Implemented pilot input pacing.** The first Washoi behavior walks forward while its own entity is present and alive. The single-bot socket loop observes snapshots and wakes at the 8 ms input cadence. It caps ordinary catch-up at four commands, discards debt above 25 ticks, and pauses movement after 500 ms without a usable observation. A clock-controlled test covers those limits. The pilot temporarily paces input in the connection loop. Multi-bot integration will use the shared scheduler.

**Verified the first live bot.** Build with `cargo build --features washoi`. A test host can include `"pilot":{"endpoint":"ws://127.0.0.1:7777","name":"SHAGADELIC"}` in its version 1 initialization record. The pilot accepts optional `ticket` and `team` fields. It reports `pilot_admitted` after entering play and `pilot_ended` on disconnect or error. Host shutdown or owner pipe closure cancels the connection.

The live movement test passed on Windows against the TypeScript Washoi server on `washoi_dm2`. With a local standalone server running, set `SHAGADELIC_TEST_WASHOI` to its WebSocket endpoint and run `cargo test --features washoi --test washoi_live -- --ignored`. The test requires input acknowledgments and horizontal displacement in server snapshots. The standalone pilot also passed admission and explicit host shutdown against that server.

**Implemented the shared Washoi session.** `WashoiSession` implements `GameSession` and links each admission attempt and socket task to a runtime entity generation. The scheduler sends input opportunities through one-slot mailboxes. A slow socket won't accumulate an unbounded input queue. Socket tasks retain their own snapshot histories and complete each ACK before resuming their event loop.

**Implemented dynamic Washoi control.** Use `"washoi_endpoint":"ws://127.0.0.1:7777"` in the initialization record to start an empty session. A `spawn` record carries `id`, `name`, and optional `ticket` and `team`. The child reports `bot_status` with the generation and pending or active status, then `bot_admitted` with the playing ID or `bot_ended` with a reason. A `remove` record addresses an ID and generation. Invalid requests receive `bot_request_failed`. A repeated spawn for a live ID returns its current status. A replacement gets a new generation. The Node control suite exercised two simultaneous bots, removal, and replacement against the live server. Set `SHAGADELIC_TEST_WASHOI` to include this case when running the control suite with the Washoi-enabled binary.

The bot suppresses movement on death and waits 2.5 seconds before sending a respawn request. It retries at the same interval while fresh snapshots still show it as dead and resets pacing on confirmed respawn. A clock-controlled test covers the dead, stale, and revived states.

The live movement test also passed against a server configured with a match admission manifest containing a bot seat. Set `SHAGADELIC_TEST_WASHOI_TICKET` for that test when the local server requires a ticket. The test hands it to the join encoder without printing it.

**Implemented the Node match supervisor.** `tools/washoi-supervisor.mjs` starts one child per match ID, waits for the versioned readiness record, and forwards existing seat tickets in spawn records. It correlates removal with the reported entity generation, gives the child two seconds to shut down before terminating it, and waits for process and stream closure. The tests cover empty matches, two match IDs sharing an endpoint, and disposal and shutdown with live bots. Run `node tests/washoi-supervisor.test.mjs ./target/debug/shagadelic.exe` with the Washoi-enabled binary. Set `SHAGADELIC_TEST_WASHOI` for the live test.

**Integrated the Washoi lobby on the `shagadelic-lobby` branches.** SHAG commit `21998d5c` invokes the runner on match-server readiness and stops it on game-server exit. It references Washoi commit `ae0996a`, which adapts lobby bot seats to the Node supervisor. Start the lobby with `SHAGADELIC_EXECUTABLE` set to the built binary and `SHAGADELIC_SUPERVISOR` set to the absolute path of `tools/washoi-supervisor.mjs`. The experimental runner connects to the local plain WebSocket game port. Lobby-managed matches containing Rust bots are unrated while behavior and reporting are under development. The TypeScript runner is the default.

The bundled lobby passed a Windows smoke test with a private Duel room on `washoi_dm2`. A bot invite led to match startup, child readiness, and admission with the seat ticket. The lifecycle test verifies the game-server exit callback for a match with zero bots. The live Node supervisor test verifies child reaping while a bot is still connected.

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
