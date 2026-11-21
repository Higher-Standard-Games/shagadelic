// Run under Washoi's vite-node with the assembled SHAG checkout as cwd.
import { readFile, writeFile } from "node:fs/promises";
import { resolve } from "node:path";

const args = process.argv.slice(2).filter((arg) => arg !== "--");
const MAX_NODES = 1024;
const MAX_ENTRIES = 64;
const MAX_EDGES = 32768;
const MAX_CORNERS = 32;
const MAX_FILE_BYTES = 4 * 1024 * 1024;
const learnedOption = args.indexOf("--learned");
const learnedFile = learnedOption < 0 ? undefined : args.splice(learnedOption, 2)[1];
const fromCache = args[0] === "--shag-cache";
if (args.length !== (fromCache ? 3 : 2) || (learnedOption >= 0 && !learnedFile)) {
  throw new Error("Usage: bake-washoi-walk-graph.mjs <authored-routes.json> <output.json> or --shag-cache <map> <output.json>, with optional --learned <bot-routes.json>");
}
const [input, output] = args.slice(fromCache ? 1 : 0);
const data = fromCache ? { map: input } : JSON.parse(await readFile(input, "utf8"));
if (typeof data.map !== "string" || !data.map.trim()) throw new Error("The input must identify a map");

const { loadBspCollision } = await import(resolve("src/games/fps/cli/HeadlessMapLoader.ts"));
const map = await loadBspCollision(data.map);
if (!map?.bspSha256) throw new Error("No indexed BSP for " + data.map);

const point = ([x, y, z]) => ({ x, y, z });
const coordinates = ({ x, y, z } = {}) => [x, y, z];
const distance = (a, b) => Math.hypot(a.x - b.x, a.z - b.z);
async function walkTest() {
  const { BotVision } = await import(resolve("src/games/fps/bot/BotVision.ts"));
  const vision = new BotVision(map.collision, {
    mapName: data.map,
    mapFingerprint: map.bspSha256,
    spawnPoints: map.spawnPoints,
    pickupPositions: map.pickups,
    triggerVolumes: map.triggers,
  }, false);
  return (from, to) => Math.abs(from.y - to.y) <= 16
    && distance(from, to) <= 192
    && vision.isWalkable(from, to);
}

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
  const learned = new Map();
  for (const route of routes) {
    if (!Array.isArray(route.waypoints) || !route.waypoints.length) {
      throw new Error("Each route needs at least one waypoint");
    }
    entries.add(node(route.entry));
    for (const point of route.waypoints) node(point);
    const goal = node(route.waypoints.at(-1));
    goals.add(goal);
    if (route.learned_route !== undefined) {
      const id = route.learned_route;
      if (typeof id !== "string" || !id.trim() || (learned.has(goal) && learned.get(goal) !== id)) {
        throw new Error("The waypoint needs one valid learned route ID");
      }
      learned.set(goal, id);
    }
  }
  if (nodes.length > MAX_NODES || entries.size > MAX_ENTRIES) throw new Error("Too many navigation nodes or entries");
  const canWalk = await walkTest();
  const edges = [];
  const points = nodes.map(point);
  const supported = points.map((point) => {
    const floor = map.collision.trace(point, { ...point, y: point.y - 64 });
    return floor.hit && floor.normal?.y >= 0.7;
  });
  for (const [from, origin] of points.entries()) {
    if (!supported[from]) continue;
    for (const [to, target] of points.entries()) {
      if (from !== to && supported[to] && canWalk(origin, target)) edges.push([from, to]);
    }
  }
  return {
    nodes, edges, entries: [...entries], goals: [...goals],
    ...(learned.size ? { learned_goals: [...learned] } : {}),
  };
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
  const pickupGoals = [];
  const pickupById = new Map(map.pickups.map((pickup) => [pickup.id, pickup]));
  for (const { id, position, type, pickupId } of original) {
    if (type !== "spawn" && type !== "item" && type !== "floor") continue;
    // BotNavGraph.walkOrigin lifts floor and item nodes to the standing origin.
    const index = nodes.length;
    standing.set(id, index);
    nodes.push(rounded([position.x, position.y + (type === "spawn" ? 0 : 24), position.z]));
    if (type === "spawn") entries.push(index);
    if (type === "item") {
      goals.push(index);
      const pickup = pickupById.get(pickupId);
      if (pickup) {
        // The item kind (e.g. "mega" or "ra") decides how much the server lets a bot take.
        const goal = { id: pickup.id, kind: pickup.type };
        if (typeof pickup.kind === "string") goal.item = pickup.kind;
        pickupGoals.push([index, goal]);
      }
    }
  }
  const edges = [];
  for (const edge of transitions) {
    const from = standing.get(edge.from);
    const to = standing.get(edge.to);
    // Walks, including their corners, and falls off ledges need only steering.
    // Jumps, jump pads, and teleporters need execution rules the bot lacks.
    if ((edge.type !== "walk" && edge.type !== "fall") || from === undefined || to === undefined) continue;
    const via = (edge.via ?? []).map(({ x, y, z }) => rounded([x, y, z]));
    edges.push(via.length ? [from, to, via] : [from, to]);
  }
  for (const [node] of pickupGoals) {
    if (entries.length >= 32) break;
    entries.push(node);
  }
  return { nodes, edges, entries, goals, pickup_goals: pickupGoals };
}

/** Keep the navigation file compact with tenth-of-a-unit precision. */
function rounded(point) {
  return point.map((value) => Math.round(value * 10) / 10);
}

function reachable(anchors, edges, reverse = false) {
  const neighbors = new Map();
  for (const edge of edges) {
    const from = edge[reverse ? 1 : 0];
    const to = edge[reverse ? 0 : 1];
    if (!neighbors.has(from)) neighbors.set(from, []);
    neighbors.get(from).push(to);
  }
  const visited = new Set(anchors);
  for (const node of visited) {
    for (const next of neighbors.get(node) ?? []) visited.add(next);
  }
  return [...visited];
}

async function linkLearned(graph, json) {
  if (json.version !== 1 || json.map !== data.map || !Array.isArray(json.routes)) {
    throw new Error("Expected a version 1 learned route file for the same map");
  }
  const canWalk = await walkTest();
  const takeoffs = new Map(graph.learned_goals);
  const approaches = reachable(graph.entries, graph.edges);
  const exits = reachable(graph.goals.filter((id) => !takeoffs.has(id)), graph.edges, true);
  const points = graph.nodes.map(point);
  const landings = [];
  const usedIds = new Set(takeoffs.values());
  for (const route of json.routes) {
    if (landings.length >= 16 || graph.nodes.length + 2 > MAX_NODES) break;
    const start = coordinates(route.start);
    const end = coordinates(route.landing);
    if (typeof route.id !== "string" || !route.id.trim() || ![...start, ...end].every(Number.isFinite)) {
      throw new Error("Invalid learned route ID or coordinates");
    }
    if (usedIds.has(route.id)) continue;
    const from = approaches.filter((node) => canWalk(points[node], route.start));
    const to = exits.filter((node) => canWalk(route.landing, points[node]));
    if (!from.length || !to.length) continue;
    const takeoff = graph.nodes.push(start) - 1;
    const landing = graph.nodes.push(end) - 1;
    for (const node of from) graph.edges.push([node, takeoff]);
    for (const node of to) graph.edges.push([landing, node]);
    graph.goals.push(takeoff);
    takeoffs.set(takeoff, route.id);
    landings.push([landing, route.id]);
    usedIds.add(route.id);
  }
  if (!landings.length) throw new Error("No learned route has both a walkable approach and a continuation");
  graph.learned_goals = [...takeoffs];
  graph.learned_landings = landings;
  console.log(`Linked ${landings.length} learned routes with walking approaches and exits`);
}

const graph = {
  bsp_sha256: map.bspSha256,
  ...(await (fromCache ? cachedGraph() : authoredGraph(data.routes))),
};
if (learnedFile) await linkLearned(graph, JSON.parse(await readFile(learnedFile, "utf8")));
if (graph.nodes.length > MAX_NODES || graph.entries.length > MAX_ENTRIES || graph.edges.length > MAX_EDGES) {
  throw new Error("Too many navigation nodes, entries, or edges");
}
if (graph.edges.some((edge) => (edge[2]?.length ?? 0) > MAX_CORNERS)) {
  throw new Error("Too many corners on a navigation edge");
}
const result = JSON.stringify({ map: data.map, graph });
if (!graph.edges.length || !graph.entries.length || !graph.goals.length) {
  throw new Error("The graph needs walk edges, entry points, and goals");
}
if (Buffer.byteLength(result) + 1 > MAX_FILE_BYTES) throw new Error("The navigation file is too large");
await writeFile(output, result + "\n");
console.log(`Wrote ${graph.nodes.length} nodes and ${graph.edges.length} walk edges for ${data.map}`);
