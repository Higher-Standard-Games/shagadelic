// Run under Washoi's vite-node with the assembled SHAG checkout as cwd.
//
// A Rust SHAGADELIC bot against a TypeScript bot, head to head over many
// headless Duels. The match setup follows SHAG `src/games/fps/cli/bot-ab.ts`
// (`bot:ab`): an in-process GameServer per match at 125 Hz with 60 Hz
// snapshots, 15 s pickup respawns, warmup expired once both bots are in, and
// the score read from the scoreboard. The TypeScript arm joins through
// `BotServer`. The Rust arm joins through `tools/washoi-supervisor.mjs`,
// with the map's navigation and sight files from a navigation directory.
// Join order alternates by match.
//
//   node node_modules/vite-node/vite-node.mjs <SHAGADELIC>/tools/bench-washoi-ab.mjs -- \
//     --exe <shagadelic.exe> --nav <navigation dir> [--map washoi_dm2] [--matches 10] \
//     [--seconds 120] [--difficulty hard] [--port 8300] [--ts '<BotOptions JSON>']
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

const argv = process.argv.slice(2).filter((arg) => arg !== "--");
const option = (name, fallback) => {
  const at = argv.indexOf(`--${name}`);
  return at < 0 ? fallback : argv[at + 1];
};
const exe = option("exe");
const navigationDir = option("nav");
if (!exe || !navigationDir) {
  throw new Error("Usage: bench-washoi-ab.mjs --exe <shagadelic.exe> --nav <navigation dir> [--map] [--matches] [--seconds] [--difficulty] [--port] [--ts]");
}
const map = option("map", "washoi_dm2");
const matches = Number(option("matches", "10"));
const seconds = Number(option("seconds", "120"));
const difficulty = option("difficulty", "hard");
const basePort = Number(option("port", "8300"));
const tsOptions = { difficulty, chat: false, ...JSON.parse(option("ts", "{}")) };

const here = new URL(".", import.meta.url);
const { WashoiSupervisor } = await import(new URL("washoi-supervisor.mjs", here).href);
const { loadBspCollision } = await import(pathToFileURL(resolve("src/games/fps/cli/HeadlessMapLoader.ts")).href);
const { GameServer } = await import(pathToFileURL(resolve("src/games/fps/server/GameServer.ts")).href);
const { Duel } = await import(pathToFileURL(resolve("src/games/fps/server/gametypes/Duel.ts")).href);
const { BotServer } = await import(pathToFileURL(resolve("src/games/fps/bot/BotServer.ts")).href);
const { MatchPhase } = await import(pathToFileURL(resolve("src/shared/server/gametype/index.ts")).href);
const { signTest } = await import(pathToFileURL(resolve("src/games/fps/bot/AbStats.ts")).href);

const wait = (ms) => new Promise((done) => setTimeout(done, ms));
const bsp = await loadBspCollision(map, "./game");
if (!bsp) throw new Error(`Could not load map "${map}"`);

async function playMatch(index, port, rustFirst) {
  const server = new GameServer(
    {
      port,
      serverName: "bench-washoi-ab",
      mapName: map,
      maxClients: 4,
      tickRate: 125,
      snapshotRate: 60,
      certPath: "",
      keyPath: "",
    },
    new Duel(),
  );
  server.setCollisionWorld(bsp.collision);
  server.setSpawnPoints(bsp.spawnPoints, bsp.teamSpawnPoints);
  server.setMapData(bsp.entities, bsp.models);
  server.setPickupItems(bsp.pickups);
  server.setTriggerVolumes(bsp.triggers);
  server.setPickupRespawnTime(15);
  if (bsp.pvs) server.setPVSData(bsp.pvs);
  await server.start();
  const endpoint = `ws://127.0.0.1:${port}`;
  const botServer = new BotServer(port + 50, {});
  const supervisor = new WashoiSupervisor(exe, { navigationDir });
  const matchId = `bench-${index}`;
  let tsBot;
  let rustBot;
  try {
    await supervisor.start(matchId, endpoint, map);
    const joinRust = async () => {
      rustBot = await supervisor.launch(matchId, { name: "SHAGADELIC Rust", difficulty });
    };
    const joinTs = async () => {
      tsBot = await botServer.addBot(endpoint, tsOptions);
    };
    for (const [i, join] of (rustFirst ? [joinRust, joinTs] : [joinTs, joinRust]).entries()) {
      if (i > 0) await wait(400);
      await join();
    }
    await wait(2000);
    const state = server.getGametypeController().getMatchState();
    state.expireWarmup();
    for (let i = 0; i < 100 && state.phase !== MatchPhase.Active; i++) await wait(100);
    await wait(seconds * 1000);
    // As bot:ab: read the scoreboard the TypeScript bot sees, by player ID.
    const board = tsBot._gameState.scores;
    const row = (id) => board.get(String(id)) ?? { score: 0, deaths: 0 };
    const rust = row(rustBot.playerId);
    const ts = row(tsBot._gameState.selfPlayerId);
    return { index, rustFirst, rust: { frags: rust.score, deaths: rust.deaths }, ts: { frags: ts.score, deaths: ts.deaths } };
  } finally {
    await rustBot?.dispose().catch(() => {});
    await supervisor.stopAll().catch(() => {});
    await botServer.removeAllBots().catch(() => {});
    await server.stop();
  }
}

const results = [];
const print = console.log;
console.log = () => {};
for (let m = 0; m < matches; m++) {
  const result = await playMatch(m, basePort + m * 2, m % 2 === 0);
  results.push(result);
  print(JSON.stringify(result));
}
console.log = print;

const gaps = results.map((r) => r.rust.frags - r.ts.frags);
const mean = gaps.reduce((a, b) => a + b, 0) / gaps.length;
const sd = Math.sqrt(gaps.reduce((a, g) => a + (g - mean) ** 2, 0) / Math.max(1, gaps.length - 1));
const half = (1.96 * sd) / Math.sqrt(gaps.length);
const wins = gaps.filter((g) => g > 0).length;
const losses = gaps.filter((g) => g < 0).length;
const sum = (pick) => results.reduce((a, r) => a + pick(r), 0);
print(`\n${map} Duel, ${seconds} s, ${matches} matches, ${difficulty}: Rust vs TypeScript`);
print(`  wins Rust ${wins}, TypeScript ${losses}, draws ${matches - wins - losses}; sign test p = ${signTest(wins, losses).toFixed(3)}`);
print(`  frag gap per match (Rust - TS): ${mean.toFixed(2)} ± ${half.toFixed(2)} (95%)`);
print(`  frags Rust ${sum((r) => r.rust.frags)}, TS ${sum((r) => r.ts.frags)}; deaths Rust ${sum((r) => r.rust.deaths)}, TS ${sum((r) => r.ts.deaths)}`);
process.exit(0);
