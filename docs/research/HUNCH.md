# HUNCH plan

[SHAGADELIC.md](SHAGADELIC.md#hunch-a-small-learned-decision-policy) records what HUNCH is for and its first experiment. This document turns that into work: what exists now, the first decision HUNCH learns, the data, the tools, the runtime, and the order to build them in. Status as of 2026-11-22, written while the TypeScript parity work finished.

## Where we start

The parity work left most of the instrumentation HUNCH needs in place.

| Piece | Where | What it gives HUNCH |
| --- | --- | --- |
| Goal arbiter | `agents/player/strategy/arbiter.rs`, `considerations.rs`, `planner.rs` | Every decision already lists eligible candidates with stable IDs, the gates that removed others, and nine named consideration scores per candidate. That is a feature vector per candidate and a rule baseline. |
| Roam choice | `BotPlayer::hold_ground` | Item, hunt a teammate's call, or hold ground, in the arbiter's currency. |
| Strategist | `strategy/strategist.rs`, `match_read.rs` | Match context per decision: diagnosis, strategy, stacks, fights, score. |
| Decision traces | `agents/player/trace.rs`, `BotPlayer::with_trace` | One serializable record per decision: state, goal, three best candidates, gated count, explanation, strategy, vitals. |
| Per-entity seeds | `random.rs` | Replays reproduce a bot's decisions, including mistakes. |
| Demo reader | `shagger_shared::demo::DemoReader` | Reads `.shagdemo` files: full snapshots per frame, plus recorded inputs from v3. Decodes through the same `EventCodec` the bot uses. |
| Recorded matches | SHAG `.cache/**.shagdemo` | 2,838 demos, nearly all bot soak and benchmark runs. `config/bot-intel/people.json` was built from two human players in two demos. Human data is scarce. |
| A/B harness | `tools/bench-washoi-ab.mjs` | Rust against TypeScript, alternating join order, sign test and frag gap. |

### First data audit (H0, 2026-11-22)

A header scan of the 1,137 uncompressed `.shagdemo` files under SHAG's `.cache` (`.shagdemo.zip` not yet included) counted players whose names are not bot names. Duel has the most of it:

| Mode and map | Demos with a non-bot player | Their minutes | Distinct non-bot names |
| --- | --- | --- | --- |
| Duel, `washoi_dm2` | 164 of 200 | 614 | 23 |
| Duel, `wfdm5` | 120 of 128 | 530 | 10 |
| Duel, `wfdm11` | 87 of 111 | 345 | 11 |
| Duel, `washoi_dm5` | 42 of 93 | 231 | 4 |
| Deathmatch, `washoi_dm2` | 77 of 91 | 144 | 11 |
| S&D, `washoi_snd1` | 21 of 28 | 117 | 2 |

Most non-bot names are the default `Player_<n>`, which test clients and probes also use, and only a few are chosen names. The 614 Duel minutes on `washoi_dm2` are an upper bound on human play. H0 still has to separate people from automated clients, for example by recorded inputs (v3+ demos) and by movement statistics, and count the `.zip` archives. Even the upper bound is about 10 hours on the first map, below the 20 hours this plan wants, so expect to pair a collection drive with a cold start.

## Decision zero: what to go for

The SHAGADELIC design starts with a tactic plus position in Duel. Start one step narrower, with the choice the arbiter already makes: which goal to pursue when not fighting. Its candidates are the pickups up or due back, orders, a teammate's call to hunt, and ground to hold.

- The candidates, gates, and features exist and are traced today. HUNCH v0 needs no new candidate builder.
- Labels come from what a human did next. A human who takes the red armor 6 seconds later, without detours, chose red armor. The `item_pickup` event names the picker, so pickups give labels without manual review. Holding and hunting need a movement rule and are labelled less confidently.
- The rule baseline is the TypeScript arbiter's choice on the same observation, so disagreement review has a reference.

v1 then adds the fight-side tactics from the SHAGADELIC table: attack here, move to attack, break contact, and pursue. Those candidates need new builders: firing positions, escape points (`movement/escape.rs`), and intercepts.

### Feature schema v0

One fixed-order vector per candidate, versioned as `washoi.goal.v0`:

1. Candidate kind, one-hot: pickup, order, hunt, hold.
2. The nine raw consideration scores, in `considerations::ALL` order. Use raw scores, not weighted ones, so that the model learns weights instead of inheriting them.
3. Route cost in world units, log-scaled, and a reachable flag.
4. Item facts for pickups: major item, health gain, armor gain, ammo need, owns the weapon, and time until respawn for timed items.
5. Interactions: health deficit × route cost, health deficit × danger, enemy stack estimate minus own stack × route cost, and, for timed items, time to respawn minus travel time.
6. Context features shared by every candidate. These rank nothing on their own, as SHAGADELIC notes, so they enter only through interactions. Examples are the mode's weights, the strategy, and the score gap.

Normalization parameters come from training data only and are stored with the model.

### Perception parity

A demo stores the full world per frame, not what one player could see. Features must come from what the bot would have perceived from that player's seat. The extractor therefore feeds each frame through the same path as a live bot: `hosting::net::player::observe`, then `BotPlayer::observe_at`, with sight, hearing, estimates, and pickup memory. It never reads the raw snapshot. This follows the AGENTS.md rule to limit bot decisions to perceivable information, and it is the main source of leakage to test for.

## Data and labels

1. Find the human matches. Filter demos by header player names against `chat::is_bot_name`, and by gametype. Count Duel minutes per map. If there are under about 20 human Duel hours, plan for a data collection drive with players opted into recording, or begin by imitating the strongest TypeScript bot settings as a cold start.
2. Choose decision moments. For every human life, take a decision point every 0.5 s of game time while the player is not in a fight. Also take one at each meaningful event: spawn, a major item seen taken, or an enemy heard.
3. Assign the label. It is the first candidate the player commits to within a horizon of 10 s: taking its pickup, reaching its order area, closing half the distance to a called place, or staying on high ground for 3 s. Ambiguous, or missing a candidate, marks the record unlabelled. Unlabelled records stay in the data for diagnosis.
4. Store outcomes apart from features: frag, death, and stack change over the next 30 s, kept for evaluation and never as training input.

Split by whole match before any normalization: 70 % train, 15 % validation, 15 % untouched test. Record the seed and the split in the dataset manifest.

## Tools

Build them as `cargo run --features washoi --bin hunch -- <command>`. They live in this repository and reuse the bot's own code.

| Command | Contract |
| --- | --- |
| `hunch scan <demo dir>` | Lists demos with map, gametype, duration, and human players. Writes `manifest.json`. |
| `hunch extract <manifest>` | Replays each human seat through the bot's perception and planner in observe-only mode. Writes one JSON line per decision: match, life, tick, schema, candidates with features, the rule's choice, and the label with its evidence. |
| `hunch inspect <record>` | Prints the candidates, features, rule choice, and label for one decision, with the demo tick to open in the SHAG viewer. |
| `hunch train <dataset>` | Seeded softmax regression over each decision's candidates: L2, minibatches, early stopping on validation. Writes the model artifact. |
| `hunch evaluate <model> <dataset>` | On the test split: top-1 agreement with the human, compared with the rule; agreement by candidate kind; calibration; and disagreement examples with demo ticks. |

## Runtime

A game-independent module, `src/hunch/`:

- `Model`: schema name, feature names in order, normalization means and scales, weights, and a version. It loads from JSON and validates dimensions, finiteness, and the schema name.
- `Model::score_into(&self, features: &[f32], width: usize, scores: &mut [f32])` uses caller-owned buffers and allocates nothing per decision.
- One immutable `Arc<Model>` per lobby, passed like the sight file: a `hunch_file` in the control init, supplied by the supervisor from a model directory.

In Washoi, the planner builds the v0 vector for each candidate it scored and asks the model for scores in three modes:

1. Off: the arbiter decides, as today.
2. Shadow: the arbiter decides, and the decision trace adds HUNCH's choice and scores. Disagreements are counted per match.
3. On: HUNCH's argmax among eligible candidates decides, ties broken by candidate ID. Gates, commitment, and failure cooldowns stay in force, so the model chooses only among what the rule allows.

A golden test scores a recorded trace in the trainer and the runtime and requires identical scores.

Status: `src/hunch.rs` holds the model, `score_into`, and the trainer. `strategy/hunch_features.rs` builds `washoi.goal.v0`, and `BotPlayer::with_hunch(model, HunchMode::Shadow | On)` wires it into the planner. The decision trace records HUNCH's pick. Still to do: passing a model through the control init and supervisor (`hunch_file`), and the extraction tool.

## Milestones

| # | Milestone | Done when |
| --- | --- | --- |
| H0 | Data audit | `hunch scan` reports human Duel hours per map. Decide on cold start or collection. |
| H1 | Extraction | `hunch extract` replays a demo seat through the bot's perception. A test checks that an enemy behind a wall in the demo is absent from that seat's features. Records parse back. |
| H2 | Labels | Pickup labels agree with manual review on 50 sampled decisions at 90 % or better. The unlabelled share is reported. |
| H3 | Trainer and runtime | `hunch train` and `src/hunch` score a fixture identically. The model artifact loads and validates. |
| H4 | First model | On held-out human matches, top-1 agreement beats the arbiter's agreement with humans. |
| H5 | Shadow | Shadow mode in live bench matches shows the disagreement rate and examples, with no added input gaps at 16 bots. |
| H6 | On | `bench-washoi-ab` against the TypeScript bot, and against the Rust arbiter, over at least 32 Duels. Adopt only if the frag gap improves and the decisions look human in blinded review. |
| H7 | Tactics | v1 candidates for fights, with the same pipeline. |

## Sessions

- Session A: H1 extraction, H3 runtime and shadow integration, and the v0 schema, because they sit on the planner and perception.
- Session B: H0 data audit and H2 labels from movement and pickups, plus the bench runs for H5 and H6, because they sit on navigation and the harness.
- Training code, H3 and H4, can go to either. It is plain Rust with no game dependency.

## Risks and open questions

- **Too little human data.** Two human players in two demos built `people.json`. If H0 finds little more, the first model learns from bot play and can at best match the rule. Collection is the user's decision.
- **Seat leakage.** Any feature read from the raw snapshot instead of the bot's perception inflates offline agreement and fails live. H1's wall test is a guard, not a proof. Review the feature list against perception for every schema change.
- **Label noise.** A human passing a pickup on the way elsewhere looks like choosing it. The detour rule in labelling and the stopover candidate in the arbiter address the same ambiguity.
- **Shift.** Humans play against humans in the demos and HUNCH plays against bots. Evaluate against both.
- **Protocol drift.** Demos record the protocol of their time, and SHAG's snapshot format is changing (`feature/server-havok-items`). Pin each dataset to the protocol its demos decode with, and keep the extractor on shagger's demo reader.
