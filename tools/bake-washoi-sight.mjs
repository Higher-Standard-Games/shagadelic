// Run under Washoi's vite-node with the assembled SHAG checkout as cwd.
//
// Exports the static geometry that blocks a Washoi bot's sight line. The bot's
// `BotVision.canSee` traces a point ray with the player content mask through
// `SweepCollisionBackend`: world brushes, clip brushes, and the collision
// facets of curved patches. This writes those brushes as plane sets and the
// patches as their tessellated triangles, in game space (y up). Doors, lifts,
// and other switchable brush models move and are left out.
//
// Format (little-endian): "SHAGSGT2", u32 header length, UTF-8 JSON header
// { map, bsp_sha256, liquids }, u32 plane count, planes as f32 [nx, ny, nz, dist],
// u32 brush count, u32 plane count per brush, u32 triangle count, triangles
// as f32 [ax, ay, az, bx, by, bz, cx, cy, cz], then the slime and lava brushes:
// u32 plane count, planes, u32 brush count, u32 plane count per brush, and u32
// contents per brush (0x8 lava, 0x10 slime). Readers also accept "SHAGSGT1",
// which ends after the triangles.
import { writeFile } from "node:fs/promises";
import { resolve } from "node:path";

const args = process.argv.slice(2).filter((arg) => arg !== "--");
const samplesOption = args.indexOf("--samples");
const samplesFile = samplesOption < 0 ? undefined : args.splice(samplesOption, 2)[1];
if (args.length !== 2 || (samplesOption >= 0 && !samplesFile)) {
  throw new Error("Usage: bake-washoi-sight.mjs <map> <output.sight> [--samples <sight-lines.json>]");
}
const [mapName, output] = args;

const { loadBspCollision } = await import(resolve("src/games/fps/cli/HeadlessMapLoader.ts"));
const map = await loadBspCollision(mapName);
if (!map?.bspSha256) throw new Error("No indexed BSP for " + mapName);

// SharedCollisionWorld -> SweepBackendAdapter -> SweepCollisionBackend.
const backend = map.collision.backend?.backend;
const bsp = backend?.bsp;
if (!bsp || !Array.isArray(backend.collisionTextures)) {
  throw new Error("The SHAG collision backend no longer has the expected shape");
}
// `PLAYER_CONTENTS`, the default mask of `BotVision.canSee`: SOLID | PLAYERCLIP.
const MASK = 0x1 | 0x10000;
const blocks = (texture) => texture && (texture.collisionMask & MASK) !== 0;
const moving = (index) => backend.liftBrushes.has(index) || backend.brushModelBrushes.has(index);

const world = bsp.models[0];
const brushIndices = new Set();
for (let i = world.firstBrush; i < world.firstBrush + world.numBrushes; i++) brushIndices.add(i);
for (const i of backend.clipBrushIndices) brushIndices.add(i);

const planes = [];
const brushPlaneCounts = [];
for (const index of [...brushIndices].sort((a, b) => a - b)) {
  const brush = bsp.brushes[index];
  if (!brush || moving(index) || !blocks(backend.getBrushTexture(brush))) continue;
  let count = 0;
  for (let side = brush.firstSide; side < brush.firstSide + brush.numSides; side++) {
    const plane = bsp.planes[bsp.brushSides[side]?.planeIndex];
    if (!plane) continue;
    // Q3 (x, y, z) is game (x, z, y).
    const [x, y, z] = plane.normal;
    planes.push(x, z, y, plane.dist);
    count++;
  }
  if (count) brushPlaneCounts.push(count);
}

// Slime and lava, `CONTENTS_SLIME` and `CONTENTS_LAVA`, which hurt a body in
// them (LiquidEscape.ts). They don't block sight, so they are kept apart.
const HARMFUL = 0x10 | 0x8;
const liquidPlanes = [];
const liquidCounts = [];
const liquidContents = [];
for (let index = world.firstBrush; index < world.firstBrush + world.numBrushes; index++) {
  const brush = bsp.brushes[index];
  const contents = backend.getBrushTexture(brush)?.contentFlags ?? 0;
  if (!brush || moving(index) || !(contents & HARMFUL)) continue;
  let count = 0;
  for (let side = brush.firstSide; side < brush.firstSide + brush.numSides; side++) {
    const plane = bsp.planes[bsp.brushSides[side]?.planeIndex];
    if (!plane) continue;
    const [x, y, z] = plane.normal;
    liquidPlanes.push(x, z, y, plane.dist);
    count++;
  }
  if (count) {
    liquidCounts.push(count);
    liquidContents.push(contents & HARMFUL);
  }
}

const triangles = [];
for (const [faceIndex, mesh] of backend.faceCollisionMeshes.entries()) {
  if (!mesh || !blocks(backend.collisionTextures[bsp.faces[faceIndex]?.textureIndex])) continue;
  for (let i = 0; i + 2 < mesh.indices.length; i += 3) {
    const corners = [0, 1, 2].map((k) => mesh.vertices[mesh.indices[i + k]]);
    if (corners.some((corner) => !corner)) continue;
    for (const { x, y, z } of corners) triangles.push(x, y, z);
  }
}

const header = Buffer.from(
  JSON.stringify({ map: mapName, bsp_sha256: map.bspSha256, liquids: liquidCounts.length }),
  "utf8",
);
const u32 = (value) => {
  const buffer = Buffer.alloc(4);
  buffer.writeUInt32LE(value);
  return buffer;
};
const floats = (values) => Buffer.from(new Float32Array(values).buffer);
await writeFile(output, Buffer.concat([
  // Version 2 adds the liquid section after the triangles. Readers accept v1.
  Buffer.from("SHAGSGT2", "ascii"),
  u32(header.length),
  header,
  u32(planes.length / 4),
  floats(planes),
  u32(brushPlaneCounts.length),
  Buffer.from(new Uint32Array(brushPlaneCounts).buffer),
  u32(triangles.length / 9),
  floats(triangles),
  u32(liquidPlanes.length / 4),
  floats(liquidPlanes),
  u32(liquidCounts.length),
  Buffer.from(new Uint32Array(liquidCounts).buffer),
  Buffer.from(new Uint32Array(liquidContents).buffer),
]));
console.log(`Wrote ${brushPlaneCounts.length} brushes, ${planes.length / 4} planes, ${triangles.length / 9} triangles, and ${liquidCounts.length} slime or lava brushes to ${output}`);

if (samplesFile) {
  // Sight lines between spawns and pickups at eye and knee height, answered by
  // SHAG's own trace. The low ones graze floors, railings, and curves.
  const points = [...map.spawnPoints, ...map.pickups.map((pickup) => pickup.position)]
    .filter((point) => point && [point.x, point.y, point.z].every(Number.isFinite))
    .flatMap(({ x, y, z }) => [32, 8].map((height) => ({ x, y: y + height, z })));
  const lines = [];
  for (const [i, from] of points.entries()) {
    for (const to of points.slice(i + 1)) {
      const rounded = (point) => [point.x, point.y, point.z].map((value) => Math.round(value * 10) / 10);
      const [a, b] = [rounded(from), rounded(to)];
      const trace = map.collision.trace({ x: a[0], y: a[1], z: a[2] }, { x: b[0], y: b[1], z: b[2] });
      lines.push([a, b, !trace.hit]);
    }
  }
  await writeFile(samplesFile, JSON.stringify({ map: mapName, lines }) + "\n");
  console.log(`Wrote ${lines.length} sight lines to ${samplesFile}`);
}
