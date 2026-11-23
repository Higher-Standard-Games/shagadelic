// Run under Washoi's vite-node with the assembled SHAG checkout as cwd.
//
// The parity-milestone bench: Rust SHAGADELIC bots against SHAG's
// TypeScript bots in Duel, FFA, and Search & Destroy, for one or two Rust
// builds in interleaved blocks. Each match is an in-process GameServer at
// 125 Hz with 60 Hz snapshots, as `bench-washoi-ab.mjs` and SHAG's
// `bot:ab` set one up, with warmup expired once every bot is in. Scores come
// from the server's own match state.
//
//   Duel  (--duel-map, default washoi_dm2): one Rust bot against one
//         TypeScript bot. Wins, the frag gap with a 95% interval, and deaths
//         the TypeScript bot didn't cause.
//   FFA   (--ffa-map, default washoi_dm2): two Rust and two TypeScript bots
//         in Deathmatch. Each bot's rank and frags.
//   S&D   (--snd-map, default washoi_snd1): a Rust pair against a
//         TypeScript pair, swapping teams each match. Rounds won by side,
//         plants, defuses, and carrier deaths.
//
// With SHAGADELIC_TRACE_DIR-style decision traces kept per arm, mode, and
// match, the summary adds trace statistics for each arm: the share of moving
// decisions at 375 u/s or more, the movers that steered, the unstick share,
// the share of fighting decisions that fired, and decision-time percentiles
// from `decide_micros`. Traces from builds before ca0754d record no firing,
// and older ones no decision time.
//
//   node node_modules/vite-node/vite-node.mjs <SHAGADELIC>/tools/bench-washoi-milestone.mjs -- \
//     --exe-a <shagadelic.exe> [--exe-b <shagadelic.exe>] --nav <navigation dir> \
//     [--learned <learned routes dir>] [--modes duel,ffa,snd] [--blocks 2] [--block 4] \
//     [--seconds 90] [--snd-seconds 240] [--difficulty hard] [--port 8700] \
//     [--traces <dir>] [--out <results.jsonl>]
//
// The navigation directory holds each map's baked files (<map>.json,
// .sight, .collision, .locations.json). A block plays --block matches of
// every mode for each arm, in an order that alternates by block; --blocks
// blocks make the run. Ports run from --port: each match takes one for the
// server and one 50 above for the TypeScript bot server, and 20 are cycled.
import { appendFileSync, existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const argv = process.argv.slice(2).filter((arg) => arg !== "--");
const option = (name, fallback) => {
  const at = argv.indexOf(`--${name}`);
  return at < 0 ? fallback : argv[at + 1];
};
const arms = [["a", option("exe-a")], ["b", option("exe-b")]].filter(([, exe]) => exe);
const navigationDir = option("nav");
if (arms.length === 0 || !navigationDir) {
  throw new Error("Usage: bench-washoi-milestone.mjs --exe-a <exe> [--exe-b <exe>] --nav <dir> [--learned <dir>] [--modes duel,ffa,snd] [--blocks] [--block] [--seconds] [--snd-seconds] [--difficulty] [--port] [--traces <dir>] [--out <file>]");
}
const learnedRoutesDir = option("learned", undefined);
const modes = option("modes", "duel,ffa,snd").split(",");
const blocks = Number(option("blocks", "2"));
const block = Number(option("block", "4"));
const seconds = Number(option("seconds", "90"));
const sndSeconds = Number(option("snd-seconds", "240"));
const difficulty = option("difficulty", "hard");
const basePort = Number(option("port", "8700"));
const traces = option("traces", undefined);
const out = option("out", undefined);
const maps = { duel: option("duel-map", "washoi_dm2"), ffa: option("ffa-map", "washoi_dm2"), snd: option("snd-map", "washoi_snd1") };
// A map without its bake runs bots that can't navigate, and the bench would
// score them anyway.
for (const mode of modes) {
  for (const extension of ["json", "sight", "collision"]) {
    const file = join(navigationDir, `${maps[mode]}.${extension}`);
    if (!existsSync(file)) throw new Error(`${mode} needs ${file}; bake ${maps[mode]} into --nav first`);
  }
}

const here = new URL(".", import.meta.url);
const load = (path) => import(pathToFileURL(resolve(path)).href);
const { WashoiSupervisor } = await import(new URL("washoi-supervisor.mjs", here).href);
const { loadBspCollision } = await load("src/games/fps/cli/HeadlessMapLoader.ts");
const { GameServer } = await load("src/games/fps/server/GameServer.ts");
const { Duel } = await load("src/games/fps/server/gametypes/Duel.ts");
const { Deathmatch } = await load("src/games/fps/server/gametypes/Deathmatch.ts");
const { SearchAndDestroy } = await load("src/games/fps/server/gametypes/SearchAndDestroy.ts");
const { BotServer } = await load("src/games/fps/bot/BotServer.ts");
const { MatchPhase } = await load("src/shared/server/gametype/index.ts");
const { EntityType } = await load("src/engine/server/ecs/types.ts");
const { signTest } = await load("src/games/fps/bot/AbStats.ts");

const wait = (ms) => new Promise((done) => setTimeout(done, ms));
const bsps = {};
for (const mode of modes) bsps[maps[mode]] ??= await loadBspCollision(maps[mode], "./game");
let portSlot = 0;

/** One match: the server, both kinds of bot, and what the mode measures. */
async function playMatch({ arm, exe, mode, index }) {
  const map = maps[mode];
  const bsp = bsps[map];
  const port = basePort + (portSlot++ % 20) * 2;
  const Gametype = { duel: Duel, ffa: Deathmatch, snd: SearchAndDestroy }[mode];
  const gametype = new Gametype();
  const server = new GameServer(
    { port, serverName: "bench-washoi-milestone", mapName: map, maxClients: 8, tickRate: 125, snapshotRate: 60, certPath: "", keyPath: "" },
    gametype,
  );
  server.setCollisionWorld(bsp.collision);
  server.setSpawnPoints(bsp.spawnPoints, bsp.teamSpawnPoints);
  server.setMapData(bsp.entities, bsp.models);
  server.setPickupItems(bsp.pickups);
  server.setTriggerVolumes(bsp.triggers);
  server.setPickupRespawnTime(15);
  if (bsp.pvs) server.setPVSData(bsp.pvs);
  server.startRecording = () => {};
  await server.start();
  const endpoint = `ws://127.0.0.1:${port}`;
  const botServer = new BotServer(port + 50, {});
  const traceDir = traces ? join(traces, arm, mode, String(index)) : undefined;
  if (traceDir) mkdirSync(traceDir, { recursive: true });
  // The child inherits its environment when the supervisor starts it.
  if (traceDir) process.env.SHAGADELIC_TRACE_DIR = traceDir;
  else delete process.env.SHAGADELIC_TRACE_DIR;
  const supervisor = new WashoiSupervisor(exe, { navigationDir, learnedRoutesDir });
  const matchId = `milestone-${arm}-${mode}-${index}`;
  const rust = [];
  const ts = [];
  // S&D swaps teams each match; the others have no teams.
  const rustTeam = index % 2 === 0 ? "soba" : "udon";
  const tsTeam = rustTeam === "soba" ? "udon" : "soba";
  const count = mode === "duel" ? 1 : 2;
  try {
    await supervisor.start(matchId, endpoint, map);
    const joins = [];
    for (let i = 0; i < count; i++) {
      joins.push(async () => {
        const bot = await supervisor.launch(matchId, {
          name: `SHAGADELIC ${i}`,
          difficulty,
          ...(mode === "snd" ? { team: rustTeam } : {}),
        });
        rust.push(String(bot.playerId));
      });
      joins.push(async () => {
        const bot = await botServer.addBot(endpoint, { difficulty, chat: false, ...(mode === "snd" ? { team: tsTeam } : {}) });
        ts.push(bot);
      });
    }
    // Alternate who joins first.
    for (const [i, join] of (index % 2 === 0 ? joins : [...joins].reverse()).entries()) {
      if (i > 0) await wait(300);
      await join();
    }
    await wait(2000);
    const state = server.getGametypeController().getMatchState();
    state.expireWarmup();
    for (let i = 0; i < 100 && state.phase !== MatchPhase.Active; i++) await wait(100);

    const tsIds = ts.map((bot) => String(bot._gameState?.selfPlayerId));
    const sim = server.simulation ?? server._simulation;
    const snd = { rounds: [], plants: { rust: 0, ts: 0 }, carries: { rust: 0, ts: 0 }, carrierDeaths: { rust: 0, ts: 0 } };
    let lastResult = null;
    let carrier = null;
    let armed = false;
    const sampler = mode === "snd" ? setInterval(() => {
      const result = state.roundResult;
      if (result && result !== lastResult) {
        lastResult = result;
        const attackers = gametype.attackers ?? null;
        snd.rounds.push({ winner: result.winner === rustTeam ? "rust" : "ts", key: result.resultKey ?? result.reason, rustAttacked: attackers === rustTeam });
      }
      const bomb = gametype.bombState;
      const players = [...(sim?.getState?.()?.entitiesByType(EntityType.Player) ?? [])];
      const owner = (id) => {
        const entity = players.find((e) => e.id === id);
        return entity ? { side: rust.includes(`player_${entity.ownerId}`) ? "rust" : "ts", dead: !!entity.health?.dead } : null;
      };
      if (bomb?.carrierId != null && bomb.carrierId !== carrier) {
        carrier = bomb.carrierId;
        const who = owner(carrier);
        if (who) snd.carries[who.side]++;
      }
      if (carrier != null) {
        const who = owner(carrier);
        if (who?.dead) {
          snd.carrierDeaths[who.side]++;
          carrier = null;
        } else if (bomb?.carrierId == null) carrier = null;
      }
      const nowArmed = gametype.getRoundState?.() === "post_plant";
      if (nowArmed && !armed) {
        const attackers = gametype.attackers ?? null;
        snd.plants[attackers === rustTeam ? "rust" : "ts"]++;
      }
      armed = nowArmed;
    }, 100) : null;
    await wait((mode === "snd" ? sndSeconds : seconds) * 1000);
    if (sampler) clearInterval(sampler);

    // The server keys its scores by number; clients see `player_<n>`.
    const key = (id) => {
      const number = Number(String(id).replace(/^player_/, ""));
      return state.scores.has(number) || state.deaths.has(number) ? number : id;
    };
    const score = (id) => ({ frags: state.scores.get(key(id)) ?? 0, deaths: state.deaths.get(key(id)) ?? 0 });
    const result = { arm, mode, map, index };
    if (mode === "duel") {
      const [r] = rust.map(score);
      const [t] = tsIds.map(score);
      Object.assign(result, { rust: r, ts: t, rustNonTsDeaths: Math.max(0, r.deaths - t.frags) });
    } else if (mode === "ffa") {
      const all = [...rust.map((id) => ({ side: "rust", ...score(id) })), ...tsIds.map((id) => ({ side: "ts", ...score(id) }))];
      all.sort((x, y) => y.frags - x.frags || x.deaths - y.deaths);
      all.forEach((bot, i) => (bot.rank = i + 1));
      result.bots = all;
    } else {
      result.snd = snd;
      result.rustTeam = rustTeam;
    }
    if (traceDir) result.traceDir = traceDir;
    return result;
  } finally {
    await supervisor.stopAll().catch(() => {});
    await botServer.removeAllBots().catch(() => {});
    await server.stop();
  }
}

/** Movement and fighting statistics from one arm's decision traces. */
function traceStats(dirs) {
  let moving = 0, fast = 0, decisions = 0, unstick = 0, fighting = 0, fired = 0;
  const micros = [];
  const movers = {};
  for (const dir of dirs) {
    if (!existsSync(dir)) continue;
    for (const file of readdirSync(dir).filter((f) => f.endsWith(".jsonl"))) {
      for (const line of readFileSync(join(dir, file), "utf8").split("\n")) {
        if (!line) continue;
        let record;
        try {
          record = JSON.parse(line);
        } catch {
          continue;
        }
        if (record.health !== undefined && record.health <= 0) continue;
        decisions++;
        if (Number.isFinite(record.decide_micros)) micros.push(record.decide_micros);
        movers[record.mover] = (movers[record.mover] ?? 0) + 1;
        if (record.mover === "unstick") unstick++;
        if (record.speed > 50) {
          moving++;
          if (record.speed >= 375) fast++;
        }
        if (record.engaged) {
          fighting++;
          if (record.firing) fired++;
        }
      }
    }
  }
  const share = (n, d) => (d ? ((100 * n) / d).toFixed(1) + "%" : "-");
  const top = Object.entries(movers).sort((a, b) => b[1] - a[1]).slice(0, 8).map(([m, n]) => `${m} ${share(n, decisions)}`).join(", ");
  micros.sort((a, b) => a - b);
  const pct = (p) => (micros.length ? `${micros[Math.min(micros.length - 1, Math.floor((p / 100) * micros.length))]} µs` : "-");
  return `${decisions} decisions; at 375 u/s or more ${share(fast, moving)} of moving; unstick ${share(unstick, decisions)}; fired in ${share(fired, fighting)} of fighting; decision time p50 ${pct(50)}, p95 ${pct(95)}, p99 ${pct(99)}; movers ${top}`;
}

const results = [];
const print = console.log;
console.log = () => {};
if (out) {
  mkdirSync(dirname(resolve(out)), { recursive: true });
  writeFileSync(out, "");
}
for (let b = 0; b < blocks; b++) {
  const order = b % 2 === 0 ? arms : [...arms].reverse();
  for (const [arm, exe] of order) {
    for (const mode of modes) {
      for (let m = 0; m < block; m++) {
        const index = b * block + m;
        const result = await playMatch({ arm, exe, mode, index });
        results.push(result);
        print(JSON.stringify(result));
        if (out) appendFileSync(out, JSON.stringify(result) + "\n");
      }
    }
  }
}
console.log = print;

const sum = (list, pick) => list.reduce((a, r) => a + pick(r), 0);
const mean = (xs) => (xs.length ? xs.reduce((a, b) => a + b, 0) / xs.length : 0);
const interval = (xs) => {
  const m = mean(xs);
  const sd = Math.sqrt(xs.reduce((a, x) => a + (x - m) ** 2, 0) / Math.max(1, xs.length - 1));
  return `${m.toFixed(2)} ± ${((1.96 * sd) / Math.sqrt(Math.max(1, xs.length))).toFixed(2)} (95%)`;
};
print(`\nMilestone bench, ${difficulty}, ${blocks} blocks of ${block} per mode per arm`);
for (const [arm, exe] of arms) {
  print(`\nArm ${arm}: ${exe}`);
  const mine = results.filter((r) => r.arm === arm);
  const duel = mine.filter((r) => r.mode === "duel");
  if (duel.length) {
    const gaps = duel.map((r) => r.rust.frags - r.ts.frags);
    const wins = gaps.filter((g) => g > 0).length;
    const losses = gaps.filter((g) => g < 0).length;
    print(`  Duel ${maps.duel}, ${duel.length} matches: wins Rust ${wins}, TS ${losses}, draws ${duel.length - wins - losses}; sign test p = ${signTest(wins, losses).toFixed(3)}`);
    print(`    frag gap (Rust - TS) ${interval(gaps)}; deaths not by the TS bot ${sum(duel, (r) => r.rustNonTsDeaths)}`);
  }
  const ffa = mine.filter((r) => r.mode === "ffa");
  if (ffa.length) {
    const bots = ffa.flatMap((r) => r.bots);
    const side = (s) => bots.filter((x) => x.side === s);
    print(`  FFA ${maps.ffa}, ${ffa.length} matches: mean rank Rust ${mean(side("rust").map((x) => x.rank)).toFixed(2)}, TS ${mean(side("ts").map((x) => x.rank)).toFixed(2)}; frags per bot Rust ${mean(side("rust").map((x) => x.frags)).toFixed(1)}, TS ${mean(side("ts").map((x) => x.frags)).toFixed(1)}; firsts Rust ${side("rust").filter((x) => x.rank === 1).length} of ${ffa.length}`);
  }
  const snd = mine.filter((r) => r.mode === "snd");
  if (snd.length) {
    const rounds = snd.flatMap((r) => r.snd.rounds);
    const won = (side, attacking) => rounds.filter((x) => x.winner === side && (x.rustAttacked === attacking) === (side === "rust")).length;
    const played = (attacking) => rounds.filter((x) => x.rustAttacked === attacking).length;
    const keys = {};
    for (const x of rounds) keys[`${x.winner} by ${x.key}`] = (keys[`${x.winner} by ${x.key}`] ?? 0) + 1;
    const total = (pick) => sum(snd, (r) => pick(r.snd));
    print(`  S&D ${maps.snd}, ${snd.length} matches, ${rounds.length} rounds: Rust won ${won("rust", true)} of ${played(true)} attacking and ${won("rust", false)} of ${played(false)} defending`);
    print(`    plants Rust ${total((s) => s.plants.rust)}, TS ${total((s) => s.plants.ts)}; carrier deaths Rust ${total((s) => s.carrierDeaths.rust)} of ${total((s) => s.carries.rust)} carries, TS ${total((s) => s.carrierDeaths.ts)} of ${total((s) => s.carries.ts)}`);
    print(`    results ${Object.entries(keys).map(([k, n]) => `${k} ${n}`).join(", ")}`);
  }
  if (traces) {
    for (const mode of modes) {
      const dirs = mine.filter((r) => r.mode === mode && r.traceDir).map((r) => r.traceDir);
      if (dirs.length) print(`  traces ${mode}: ${traceStats(dirs)}`);
    }
  }
}
process.exit(0);
