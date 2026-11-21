// Run under Washoi's vite-node with the assembled SHAG checkout as cwd.
//
// Exports a map's location names for team callouts: the `target_location`
// entities, as `shared/bsp/LocationTable.ts` `extractLocations` reads them
// for the TypeScript bot and the server, in game space (y up).
//
// Format: { "map": <name>, "locations": [{ "name": <plain name>, "position": [x, y, z] }] }
import { writeFile } from "node:fs/promises";
import { resolve } from "node:path";

const args = process.argv.slice(2).filter((arg) => arg !== "--");
if (args.length !== 2) throw new Error("Usage: bake-washoi-locations.mjs <map> <output.locations.json>");
const [mapName, output] = args;

const { loadBspCollision } = await import(resolve("src/games/fps/cli/HeadlessMapLoader.ts"));
const { extractLocations } = await import(resolve("src/shared/bsp/LocationTable.ts"));
const map = await loadBspCollision(mapName);
if (!map) throw new Error("No BSP for " + mapName);

const locations = extractLocations(map.entities).map(({ name, position }) => ({
  name,
  position: [position.x, position.y, position.z],
}));
await writeFile(output, `${JSON.stringify({ map: mapName, locations }, null, 2)}\n`);
console.log(`${locations.length} locations for ${mapName} in ${output}`);
