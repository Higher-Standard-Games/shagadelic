// Run under Washoi's vite-node with the assembled SHAG checkout as cwd.
import { readFile, writeFile } from "node:fs/promises";
import { resolve } from "node:path";

const args = process.argv.slice(2).filter((arg) => arg !== "--");
const fromCache = args[0] === "--shag-cache";
if (args.length !== (fromCache ? 3 : 2)) {
  throw new Error("Usage: bake-washoi-walk-graph.mjs <authored-routes.json> <output.json> or --shag-cache <map> <output.json>");
}
const [input, output] = args.slice(fromCache ? 1 : 0);
const data = fromCache ? { map: input } : JSON.parse(await readFile(input, "utf8"));
if (typeof data.map !== "string" || !data.map.trim()) throw new Error("The input must identify a map");

const { loadBspCollision } = await import(resolve("src/games/fps/cli/HeadlessMapLoader.ts"));
const map = await loadBspCollision(data.map);
if (!map?.bspSha256) throw new Error("No indexed BSP for " + data.map);

async function authoredGraph(routes) {
  if (!Array.isArray(routes) || !routes.length) throw new Error("The input must contain authored routes");
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
  for (const route of routes) {
    if (!Array.isArray(route.waypoints) || !route.waypoints.length) {
      throw new Error("Each route needs at least one waypoint");
    }
    entries.add(node(route.entry));
    for (const point of route.waypoints) node(point);
    goals.add(node(route.waypoints.at(-1)));
  }
  if (nodes.length > 256 || entries.size > 32) throw new Error("Too many navigation nodes or entries");
  const { BotVision } = await import(resolve("src/games/fps/bot/BotVision.ts"));
  const vision = new BotVision(map.collision, {
    mapName: data.map,
    mapFingerprint: map.bspSha256,
    spawnPoints: map.spawnPoints,
    pickupPositions: map.pickups,
    triggerVolumes: map.triggers,
  }, false);
  const edges = [];
  const points = nodes.map(([x, y, z]) => ({ x, y, z }));
  const supported = points.map((point) => {
    const floor = map.collision.trace(point, { ...point, y: point.y - 64 });
    return floor.hit && floor.normal?.y >= 0.7;
  });
  for (const [from, origin] of points.entries()) {
    if (!supported[from]) continue;
    for (const [to, target] of points.entries()) {
      if (from === to || !supported[to] || Math.abs(origin.y - target.y) > 16) continue;
      if (Math.hypot(origin.x - target.x, origin.z - target.z) > 192) continue;
      if (vision.isWalkable(origin, target)) edges.push([from, to]);
    }
  }
  return { nodes, edges, entries: [...entries], goals: [...goals] };
}

async function cachedGraph() {
  const { BotNavCacheStore } = await import(resolve("src/games/fps/bot/BotNavCache.ts"));
  const cache = new BotNavCacheStore();
  const saved = cache.load(data.map, map.bspSha256);
  if (!saved) throw new Error(cache.lastDiagnostic);
  const { nodes: original, edges: transitions } = saved.exportStaticData();
  const standing = new Map();
  const entries = [];
  const goals = [];
  const nodes = [];
  for (const { id, position, type } of original) {
    if (type !== "spawn" && type !== "item" && type !== "floor") continue;
    // BotNavGraph.walkOrigin lifts floor and item nodes to the standing origin.
    const index = nodes.length;
    standing.set(id, index);
    nodes.push([position.x, position.y + (type === "spawn" ? 0 : 24), position.z]);
    if (type === "spawn") entries.push(index);
    if (type === "item") goals.push(index);
  }
  const edges = [];
  for (const edge of transitions) {
    const from = standing.get(edge.from);
    const to = standing.get(edge.to);
    if (edge.type !== "walk" || edge.via?.length || from === undefined || to === undefined) continue;
    if (Math.abs(nodes[from][1] - nodes[to][1]) <= 32) edges.push([from, to]);
  }
  return { nodes, edges, entries, goals };
}

const graph = {
  bsp_sha256: map.bspSha256,
  ...(await (fromCache ? cachedGraph() : authoredGraph(data.routes))),
};
if (graph.nodes.length > 256 || graph.entries.length > 32) throw new Error("Too many navigation nodes or entries");
const result = JSON.stringify({ map: data.map, graph });
if (!graph.edges.length || !graph.entries.length || !graph.goals.length) {
  throw new Error("The graph needs walk edges, entry points, and goals");
}
if (Buffer.byteLength(result) > 60 * 1024) throw new Error("The graph is too large for an initialization record");
await writeFile(output, result + "\n");
console.log(`Wrote ${graph.nodes.length} nodes and ${graph.edges.length} walk edges for ${data.map}`);
