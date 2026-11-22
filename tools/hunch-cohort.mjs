// Run under Washoi's vite-node with a SHAG checkout as cwd:
//
//   node node_modules/vite-node/vite-node.mjs <SHAGADELIC>/tools/hunch-cohort.mjs -- \
//     <demo dir>... --out <cohort.json> [--sessions <sessions.json>] [--freeze <old cohort.json>] [--seed <n>]
//
// HUNCH's cohort (docs/research/HUNCH.md, "Next experiment", item 1): which
// recorded matches HUNCH may learn from, and how they split.
//
// Every `.shagdemo`, gzipped or not, and every `.shagdemo` inside a
// `.shagdemo.zip`, is read once. Copies with the same content, archived or
// unpacked, are one demo, identified by the SHA-256 of its decompressed
// bytes, with all its paths listed. Each seat gets a provenance:
//
//   confirmed  a session record names the seat as a person who played it
//   automated  a bot name, or a session record naming the seat as a client
//   unknown    anything else. A chosen name, a default `Player_<n>`, or a
//              verified account is evidence, not proof: test clients use
//              default names and accounts too.
//
// Only demos with a confirmed seat are eligible. Each eligible demo is split
// into train, validation, or test by its recording session, or by match where
// no session is known, so a split never shares a match and, where possible,
// never shares a session. `--freeze` keeps the split of every demo an earlier
// cohort placed, so adding recordings never moves one.
//
// A sessions file is what the person collecting fills in, one entry per
// recording session:
//
//   { "sessions": [ { "id": "2026-11-22-evening", "from": "2026-11-22T17:00:00Z",
//       "to": "2026-11-22T21:00:00Z", "humans": ["tarmo"], "automated": [],
//       "bots": { "rust": "<SHAGADELIC rev>", "ts": "<SHAG rev>" }, "notes": "" } ] }
//
// A demo belongs to the session whose window holds its start time.
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { readdirSync, readFileSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { join, resolve } from "node:path";
import { gunzipSync } from "node:zlib";

const args = process.argv.slice(2).filter((arg) => arg !== "--");
const take = (name) => {
  const at = args.indexOf(name);
  return at < 0 ? undefined : args.splice(at, 2)[1];
};
const out = take("--out");
const sessionsFile = take("--sessions");
const freezeFile = take("--freeze");
const seed = take("--seed") ?? "hunch-cohort-v0";
const roots = args;
if (!out || roots.length === 0) {
  throw new Error("Usage: hunch-cohort.mjs <demo dir>... --out <cohort.json> [--sessions <file>] [--freeze <cohort.json>] [--seed <n>]");
}

const { DemoReader } = await import(`file:///${process.cwd()}/src/shared/demo/DemoReader.ts`);
const { isBotName } = await import(`file:///${process.cwd()}/src/games/fps/shared/bot/BotNames.ts`);
const JSZip = createRequire(resolve("package.json"))("jszip");

const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");
const git = (cwd) => {
  try {
    return execFileSync("git", ["rev-parse", "HEAD"], { cwd, encoding: "utf8" }).trim();
  } catch {
    return null;
  }
};

const sessions = sessionsFile ? JSON.parse(readFileSync(sessionsFile, "utf8")).sessions ?? [] : [];
for (const session of sessions) {
  session.fromMs = Date.parse(session.from);
  session.toMs = Date.parse(session.to);
  if (!session.id || !Number.isFinite(session.fromMs) || !Number.isFinite(session.toMs)) {
    throw new Error(`session ${JSON.stringify(session.id)} needs an id and ISO from and to`);
  }
}
const group = (demo) => demo.session ? `session:${demo.session}` : `match:${demo.match_id ?? demo.sha256}`;
const frozenGroups = new Map();
const frozen = new Map();
function keepGroup(key, split) {
  if (!["train", "validation", "test"].includes(split) || (frozenGroups.has(key) && frozenGroups.get(key) !== split)) {
    throw new Error(`Conflicting or invalid frozen split for ${key}`);
  }
  frozenGroups.set(key, split);
}
if (freezeFile) {
  for (const demo of JSON.parse(readFileSync(freezeFile, "utf8")).demos ?? []) {
    if (demo.split) {
      frozen.set(demo.sha256, demo.split);
      keepGroup(group(demo), demo.split);
    }
  }
}

function* files(dir) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      if (entry.name !== "node_modules") yield* files(path);
    } else if (entry.name.endsWith(".shagdemo") || entry.name.endsWith(".shagdemo.zip")) {
      yield path;
    }
  }
}

/** Every demo's bytes in a file: itself, gunzipped, or each in an archive. */
async function* demosIn(path) {
  const raw = readFileSync(path);
  if (path.endsWith(".zip")) {
    const zip = await JSZip.loadAsync(raw);
    for (const entry of Object.values(zip.files)) {
      if (!entry.dir && entry.name.endsWith(".shagdemo")) {
        yield [`${path}!${entry.name}`, Buffer.from(await entry.async("uint8array"))];
      }
    }
  } else {
    yield [path, raw[0] === 0x1f && raw[1] === 0x8b ? gunzipSync(raw) : raw];
  }
}

const demos = new Map();
let unreadable = 0;
for (const root of roots) {
  for (const path of files(root)) {
    try {
      for await (const [where, bytes] of demosIn(path)) {
        const data = bytes[0] === 0x1f && bytes[1] === 0x8b ? gunzipSync(bytes) : bytes;
        const hash = sha256(data);
        const known = demos.get(hash);
        if (known) {
          known.paths.push(where);
          continue;
        }
        let header;
        try {
          header = new DemoReader(data).readHeader();
        } catch {
          unreadable++;
          continue;
        }
        const minutes = (data.readUInt32LE(data.length - 8) || 0) / 60000;
        const session = sessions.find((s) => s.fromMs <= header.startTime && header.startTime <= s.toMs);
        const seats = (header.players ?? []).filter((p) => p.name).map((p) => {
          const evidence = [];
          if (isBotName(p.name)) evidence.push("bot-name");
          else if (/^Player_\d+$/.test(p.name)) evidence.push("default-name");
          else evidence.push("chosen-name");
          if (p.accountId != null) evidence.push("account");
          let human = "unknown";
          if (isBotName(p.name) || session?.automated?.includes(p.name)) human = "automated";
          else if (session?.humans?.includes(p.name)) human = "confirmed";
          if (session?.humans?.includes(p.name) || session?.automated?.includes(p.name)) evidence.push("session");
          return { name: p.name, player_id: p.playerId, account_id: p.accountId ?? null, human, evidence };
        });
        demos.set(hash, {
          sha256: hash,
          paths: [where],
          bytes: data.length,
          version: header.version,
          map: header.map,
          gametype: header.gametype,
          start_time: new Date(header.startTime).toISOString(),
          match_id: header.matchId ?? null,
          minutes: Math.round(minutes * 100) / 100,
          session: session?.id ?? null,
          bots: session?.bots ?? null,
          seats,
          eligible: seats.some((seat) => seat.human === "confirmed"),
          split: null,
        });
      }
    } catch (error) { unreadable++; console.error(path, error.message); }
  }
}

// Split whole groups: a session where known, else the match.
const unit = (key) => parseInt(sha256(`${seed}|${key}`).slice(0, 8), 16) / 2 ** 32;
for (const demo of demos.values()) {
  if (frozen.has(demo.sha256)) keepGroup(group(demo), frozen.get(demo.sha256));
}
for (const demo of demos.values()) {
  if (!demo.eligible) continue;
  const key = group(demo);
  if (frozenGroups.has(key)) {
    demo.split = frozenGroups.get(key);
    continue;
  }
  const draw = unit(key);
  demo.split = draw < 0.7 ? "train" : draw < 0.85 ? "validation" : "test";
}

const list = [...demos.values()].sort((a, b) => a.start_time.localeCompare(b.start_time) || a.sha256.localeCompare(b.sha256));
const count = (pick) => list.filter(pick).length;
const cohort = {
  schema: "hunch.cohort.v0",
  seed,
  decoder: { shag: git(process.cwd()), versions: [...new Set(list.map((d) => d.version))].sort((a, b) => a - b) },
  sessions: sessionsFile ? { path: sessionsFile, sha256: sha256(readFileSync(sessionsFile)) } : null,
  frozen_from: freezeFile ?? null,
  summary: {
    files_unreadable: unreadable,
    demos: list.length,
    duplicate_copies: list.reduce((sum, d) => sum + d.paths.length - 1, 0),
    eligible: count((d) => d.eligible),
    seats: {
      confirmed: list.flatMap((d) => d.seats).filter((s) => s.human === "confirmed").length,
      automated: list.flatMap((d) => d.seats).filter((s) => s.human === "automated").length,
      unknown: list.flatMap((d) => d.seats).filter((s) => s.human === "unknown").length,
    },
    splits: Object.fromEntries(["train", "validation", "test"].map((s) => [s, count((d) => d.split === s)])),
  },
  demos: list,
};
writeFileSync(out, JSON.stringify(cohort, null, 1) + "\n");
console.log(JSON.stringify(cohort.summary));
