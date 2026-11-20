// Run under Washoi's vite-node with the assembled SHAG checkout as cwd.
import { readFile, writeFile } from "node:fs/promises";
import { resolve } from "node:path";

const args = process.argv.slice(2).filter((arg) => arg !== "--");
if (args.length !== 2) throw new Error("Usage: bake-washoi-walk-graph.mjs <authored-routes.json> <output.json>");
const [input, output] = args;
const data = JSON.parse(await readFile(input, "utf8"));
if (typeof data.map !== "string" || !Array.isArray(data.routes) || !data.routes.length) {
  throw new Error("The input must contain a map and authored routes");
}

const { loadBspCollision } = await import(resolve("src/games/fps/cli/HeadlessMapLoader.ts"));
const { BotVision } = await import(resolve("src/games/fps/bot/BotVision.ts"));
const map = await loadBspCollision(data.map);
if (!map?.bspSha256) throw new Error("No indexed BSP for " + data.map);
const vision = new BotVision(map.collision, {
  mapName: data.map,
  mapFingerprint: map.bspSha256,
  spawnPoints: map.spawnPoints,
  pickupPositions: map.pickups,
  triggerVolumes: map.triggers,
}, false);

const nodes = [];
const ids = new Map();
function node(point) {
  if (!Array.isArray(point) || point.length !== 3 || !point.every(Number.isFinite)) {
    throw new Error("Waypoints must be finite XYZ coordinates");
  }
  const key = JSON.stringify(point);
  if (ids.has(key)) return ids.get(key);
  const id = nodes.length;
  nodes.push(point);
  ids.set(key, id);
  return id;
}
const entries = new Set();
const goals = new Set();
for (const route of data.routes) {
  if (!Array.isArray(route.waypoints) || !route.waypoints.length) {
    throw new Error("Each route needs at least one waypoint");
  }
  entries.add(node(route.entry));
  for (const point of route.waypoints) node(point);
  goals.add(node(route.waypoints.at(-1)));
}
if (nodes.length > 256 || entries.size > 32) throw new Error("Too many navigation nodes or entries");

const vec = ([x, y, z]) => ({ x, y, z });
const points = nodes.map(vec);
const supported = points.map((point) => {
  const floor = map.collision.trace(point, { ...point, y: point.y - 64 });
  return floor.hit && floor.normal?.y >= 0.7;
});
const edges = [];
for (const [from, origin] of points.entries()) {
  if (!supported[from]) continue;
  for (const [to, target] of points.entries()) {
    if (from === to || !supported[to] || Math.abs(origin.y - target.y) > 16) continue;
    if (Math.hypot(origin.x - target.x, origin.z - target.z) > 192) continue;
    if (vision.isWalkable(origin, target)) edges.push([from, to]);
  }
}

const graph = {
  bsp_sha256: map.bspSha256,
  nodes,
  edges,
  entries: [...entries],
  goals: [...goals],
};
const result = JSON.stringify({ map: data.map, graph });
if (!edges.length) throw new Error("The collision checks produced no walk edges");
if (Buffer.byteLength(result) > 60 * 1024) throw new Error("The graph is too large for an initialization record");
await writeFile(output, result + "\n");
console.log(`Wrote ${nodes.length} nodes and ${edges.length} walk edges for ${data.map}`);
