# HUNCH plan

[SHAGADELIC.md](SHAGADELIC.md#hunch-a-small-learned-decision-policy) records what HUNCH is for and its first experiment. This document turns that into work: what exists now, the first decision HUNCH learns, the data, the tools, the runtime, and the order to build them in. Status as of 2026-11-22, written while the TypeScript parity work finished.

## Next experiment and handoff

Keep the Rust linear scorer for the first human experiment. Ask one question on `washoi_dm2`: given the reachable pickups at a quiet Duel decision, which one does a known human deliberately go for? Evaluate pickup choice conditional on a pickup decision. Report how many decisions qualify. Hold, hunt, and combat choices need their own evidence. The 48.3% result below measures a proxy on six held-out matches and 1,774 overlapping decision samples. It establishes a working pipeline. Human tactical judgment remains unmeasured.

1. **C prepares a reproducible cohort.** Record the demo content hash, seat, human provenance, map and asset fingerprints, decoder and bot revisions, and collection session. Deduplicate archived and unpacked copies. Keep confirmed human, automated, and unknown seats separate. A chosen name helps locate a seat; session records identify who played it. Freeze train, validation, and test lists by whole match before fitting. Hold out recording sessions where possible. If only one person contributes verified footage, describe the result as imitation of that player's style.
2. **C audits intent before retraining.** The current extractor attributes nearby pickup-mask changes before it reads named pickup events, then expires records after assigning labels. An opponent can take an item beside the observed seat, and a pickup on the first frame beyond the horizon can label an expired decision. Give named events precedence, veto a mask inference when the event names another player, enforce the horizon before assignment, and stop pending intent labels at death or round end. Store evidence type, life, pickup tick, and ambiguity. Review 50 independent pickup episodes sampled across matches and evidence types, including incidental pickups and unlabelled cases. Count one pickup episode once when estimating confidence. Publish counts and coverage alongside the H2 agreement target.
3. **A verifies what the model could see.** Replay paired fixtures through extraction and the live observation path. Changing an unseen, unheard opponent behind a wall should leave eligible candidates and feature rows unchanged until a perceptible cue arrives. Check recorded player view direction when available. Require the intended navigation and sight assets for the human benchmark. The current schema has 22 columns and two kind indicators, pickup and order. Use the ordered names in `hunch_features.rs` as the artifact contract and validate the names on load. Version extensions separately.
4. **Train once the labels pass review.** Fit normalization and weights on training matches, select regularization and the stopping epoch on validation, then score the frozen test set. Compare the rule, nearest reachable pickup, and a training-set pickup-frequency baseline on identical candidates. Report per-match agreement, eligible coverage, and the paired improvement with uncertainty resampled by match. Weight or subsample decisions leading to the same pickup so one long approach cannot dominate. Preserve the manifest, extractor command, seed, feature order, and artifact hash. Process demos one at a time and write JSONL incrementally on memory-limited machines.
5. **A wires shadow mode; C measures it.** Load a validated artifact once per lobby. Replay identical observations and seeds with shadow off and on and require identical emitted controls. In a 16-bot run record decision latency, input gaps, disagreements, and clips for human review. Only then enable the policy in private paired Duels with the Rust rule and TypeScript baseline. Treat the planned 32 Duels as a first sample; report paired outcomes and uncertainty before deciding whether to adopt it.

The next schema ranks a tactic **and its destination**. Start with clips where the player can attack here, move for a better angle, disengage for a resource, pursue, or deal damage from a losing position. Review tactic and position separately and flag a sensible option missing from the candidate set. Carry perceived damage and pickup timing across respawn with uncertainty. Keep later recovery, the frag conceded, and the next encounter in outcome fields only. This allows reviewers to judge whether a last stand bought pressure even when a later kill has several possible causes.

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

Session C reran the audit over the local SHAG `.cache` with `tools/hunch-cohort.mjs`. Of 1,140 copies read, 312 had distinct decompressed hashes. Across those matches the scanner counted 612 bot-name seats and 876 seats of unknown provenance. Without collector session records, zero seats qualified for a human train, validation, or test split. The earlier header totals above count copies. The manifest retains their paths so the same recording can be reviewed once.

Extraction now writes the pickup tick, evidence source, ambiguity flag, life, and closure reason. A named pickup takes precedence over proximity, including an attribution on the following frame. Ambiguous mask labels stay available for review and are excluded from training and evaluation. The 50-episode detour review and human agreement measurement remain open.

### First extraction and model (H1 to H4, 2026-11-22)

`hunch extract` (`src/bin/hunch.rs`) replays each non-bot seat of a demo through `BotPlayer`, using its perception, estimates, and planner, from that seat's entity. Every half second it records the eligible goal candidates with their `washoi.goal.v0` rows and the arbiter's choice. The label is the first candidate pickup the seat takes within 10 s: named by the `item_pickup` event, or seen vanishing from the pickup mask within 180 units of the seat. `hunch train` fits the model, and `hunch evaluate` compares it on held-out matches with the arbiter, the nearest candidate by walk, and uniform chance.

The first 60 `washoi_dm2` Duel demos gave 25,296 decisions from 24 seats, 8,064 of them labelled. Trained on 17 matches and tested on 6 others:

| Chooser | Agreement with the seat's next pickup, held out (1,774 decisions) |
| --- | --- |
| HUNCH v0, linear | 48.3 % |
| Goal arbiter | 20.5 % |
| Nearest by walk | 20.3 % |
| Uniform chance | 13.3 % |

This is a pipeline result, not yet evidence about people. H0 found that these seats may be test clients under default names. "Next pickup taken" also counts items picked up on the way, which inflates agreement for any chooser that likes routes through clusters. The largest weights, negative ammo need and health gain and positive item value, read like weapon-first scripted play. Next: separate people from clients (H0), weigh labels by detour (H2), and only then compare live in shadow mode (H5).

### Collection (decided 2026-11-22)

The user records human Duels against the bots. Those records give labels from a person's choices against the opponents HUNCH will face, and a direct comparison of results against each bot. Conventions for the recordings:

- A chosen player name, never the default `Player_<n>`, so `hunch extract` finds the human seat.
- `washoi_dm2` first, the map with baked navigation and sight. Other maps need `bake-washoi-walk-graph` and `bake-washoi-sight` first.
- Mostly hard or insane bots, the Rust and the TypeScript bot both, with the bot build fixed and noted per session.
- A held-out test needs a few hours. Shadow mode waits for about 20.

People learn the bots' habits, and the labels will carry that. It is the distribution HUNCH plays in, and the habits people exploit are a list of the bots' weaknesses.

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

Status: `src/hunch.rs` holds the model, `score_into`, and the trainer. `strategy/hunch_features.rs` builds `washoi.goal.v0`, and `BotPlayer::with_hunch(model, HunchMode::Shadow | On)` wires it into the planner. The decision trace records HUNCH's pick, and in `on` mode its goal is the model's. The control init loads `hunch_file` once per lobby, in `hunch_mode` `shadow` (the default) or `on`. The supervisor sets them from `SHAGADELIC_HUNCH_FILE` and `SHAGADELIC_HUNCH_MODE`. In shadow, the controls match a run with no model.

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
- Session C owns H0, H2, H4 and the H5 measurement. Session B owns movement and its live fixtures. Assign H6 after reviewing shadow results.
- Session A owns H3 runtime and schema compatibility. Session C owns the evaluation split and paired analysis.

## Risks and open questions

- **Too little human data.** Two human players in two demos built `people.json`. If H0 finds little more, the first model learns from bot play and can at best match the rule. Collection is the user's decision.
- **Seat leakage.** Any feature read from the raw snapshot instead of the bot's perception inflates offline agreement and fails live. H1's wall test is a guard, not a proof. Review the feature list against perception for every schema change.
- **Label noise.** A human passing a pickup on the way elsewhere looks like choosing it. The detour rule in labelling and the stopover candidate in the arbiter address the same ambiguity.
- **Shift.** Humans play against humans in the demos and HUNCH plays against bots. Evaluate against both.
- **Protocol drift.** SHAG's `feature/server-havok-items` moves item drops to Havok physics and changes the snapshot format. The Rust decoder follows when it ships, and demos recorded before and after need the matching decoder.
- **Pinning.** Demos record the protocol of their time. Pin each dataset to the protocol its demos decode with, and keep the extractor on shagger's demo reader.
