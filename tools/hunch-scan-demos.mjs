// Run under Washoi's vite-node with a SHAG checkout as cwd: node node_modules/vite-node/vite-node.mjs <SHAGADELIC>/tools/hunch-scan-demos.mjs -- <demo dir>. Set NAMES=1 to list names.
// H0: human players and minutes in recorded demos. Header and trailer only.
import { readFileSync } from "node:fs";
import { gunzipSync } from "node:zlib";
import { execSync } from "node:child_process";
const root = process.argv[2];
const { DemoReader } = await import(`file:///${process.cwd()}/src/shared/demo/DemoReader.ts`);
const { isBotName } = await import(`file:///${process.cwd()}/src/games/fps/shared/bot/BotNames.ts`);
const files = execSync(`find "${root}" -name "*.shagdemo" -not -path "*/node_modules/*"`, { maxBuffer: 1 << 28 }).toString().trim().split("\n");
const by = new Map();
let read = 0, failed = 0;
for (const file of files) {
  try {
    let data = readFileSync(file);
    if (data[0] === 0x1f && data[1] === 0x8b) data = gunzipSync(data);
    const reader = new DemoReader(data);
    const h = reader.readHeader();
    const minutes = (data.readUInt32LE(data.length - 8) || 0) / 60000;
    const humans = (h.players ?? []).map((p) => p.name).filter((n) => n && !isBotName(n));
    const key = `${h.gametype}|${h.map}`;
    const row = by.get(key) ?? { demos: 0, humanDemos: 0, humanMinutes: 0, names: new Set() };
    row.demos++;
    if (humans.length) { row.humanDemos++; row.humanMinutes += minutes; humans.forEach((n) => row.names.add(n)); }
    by.set(key, row);
    read++;
  } catch { failed++; }
}
console.log(`read ${read}, failed ${failed}`);
for (const [key, row] of [...by].sort((a, b) => b[1].humanMinutes - a[1].humanMinutes))
  if (process.env.NAMES) console.log(key, [...row.names].join(", "));
  console.log(key.padEnd(34), `demos ${row.demos}`.padEnd(11), `human demos ${row.humanDemos}`.padEnd(17), `human min ${row.humanMinutes.toFixed(0)}`.padEnd(15), `players ${row.names.size}`);
