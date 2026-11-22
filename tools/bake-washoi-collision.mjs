// Run under Washoi's vite-node with the assembled SHAG checkout as cwd.
//
// Writes a map's BSP, byte for byte, for the bot's collision world. Washoi's
// server traces and moves players through the map's brushes and patches
// (HeadlessMapLoader.loadBspCollision). A bot that steps Washoi's movement
// ahead of itself reads the same BSP through shagger_shared's sweep, so its
// rollouts collide where the server's do. The BSP is found through the same
// pk3 index the game uses.
//
// A BSP names no map, so the bot checks it by hash: the file's SHA-256 must
// equal the `bsp_sha256` its baked navigation records. Pass --expect-sha with
// that value to refuse a mismatch here too.
import { createHash } from "node:crypto";
import { writeFile } from "node:fs/promises";
import { resolve } from "node:path";

const args = process.argv.slice(2).filter((arg) => arg !== "--");
const expectOption = args.indexOf("--expect-sha");
const expected = expectOption < 0 ? undefined : args.splice(expectOption, 2)[1];
if (args.length !== 2 || (expectOption >= 0 && !/^[0-9a-f]{64}$/.test(expected ?? ""))) {
  throw new Error("Usage: bake-washoi-collision.mjs <map> <output.bsp> [--expect-sha <sha256 hex>]");
}
const [mapName, output] = args;

const { resolveMapBspViaIndex } = await import(resolve("src/engine/loader/pk3/NodePk3.ts"));
const { PK3Loader } = await import(resolve("src/engine/loader/pk3/PK3Loader.ts"));

// HeadlessMapLoader's candidate directories, in its order.
const candidates = [resolve("./public/data"), resolve("./dist/data"), resolve("./game")];
const bsp = await resolveMapBspViaIndex(candidates, mapName, PK3Loader.createTransient());
if (!bsp) throw new Error(`No indexed BSP for ${mapName} in ${candidates.join(", ")}`);

const bytes = new Uint8Array(bsp);
const sha = createHash("sha256").update(bytes).digest("hex");
if (expected && sha !== expected) {
  throw new Error(`${mapName} BSP is ${sha}, not the expected ${expected}`);
}
await writeFile(output, bytes);
console.log(`Wrote ${bytes.byteLength} bytes of ${mapName} (sha256 ${sha}) to ${output}`);
