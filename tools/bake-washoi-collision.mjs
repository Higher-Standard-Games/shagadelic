// Run under Washoi's vite-node with the assembled SHAG checkout as cwd.
//
// Writes a map's collision for the bot's collision world. Washoi's server
// traces and moves players through the map's brushes and patches
// (HeadlessMapLoader.loadBspCollision). A bot that steps Washoi's movement
// ahead of itself reads the same BSP lumps through shagger_shared's sweep, so
// its rollouts collide where the server's do. The BSP is found through the
// same pk3 index the game uses.
//
// The file is the BSP with the lumps collision never reads emptied
// (meshverts, lightmaps, light volumes, visibility, and FBSP's light array),
// which leaves washoi_snd1 a fraction of its 96 MB, behind a header naming the
// map and the full BSP's SHA-256:
//
//   "SHAGCOL1", u32 header length, UTF-8 JSON { map, bsp_sha256 }, the BSP.
//
// The bot refuses a file whose bsp_sha256 differs from its navigation's.
// Pass --expect-sha with that value to refuse a mismatch here too.
import { createHash } from "node:crypto";
import { writeFile } from "node:fs/promises";
import { resolve } from "node:path";

const args = process.argv.slice(2).filter((arg) => arg !== "--");
const expectOption = args.indexOf("--expect-sha");
const expected = expectOption < 0 ? undefined : args.splice(expectOption, 2)[1];
if (args.length !== 2 || (expectOption >= 0 && !/^[0-9a-f]{64}$/.test(expected ?? ""))) {
  throw new Error("Usage: bake-washoi-collision.mjs <map> <output.collision> [--expect-sha <sha256 hex>]");
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

// bsp_loader's lump numbers. FBSP has an 18th, the light array.
const KEEP = new Set([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 12, 13]);
const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
const magic = String.fromCharCode(...bytes.subarray(0, 4));
if (magic !== "IBSP" && magic !== "FBSP") throw new Error(`${mapName} is not a BSP: ${magic}`);
const count = magic === "FBSP" ? 18 : 17;
const head = 8 + 8 * count;
const lumps = Array.from({ length: count }, (_, i) => ({
  offset: view.getInt32(8 + 8 * i, true),
  length: view.getInt32(12 + 8 * i, true),
}));
const parts = [];
let at = head;
const directory = new DataView(new ArrayBuffer(8 * count));
for (const [i, { offset, length }] of lumps.entries()) {
  const kept = KEEP.has(i) && length > 0;
  if (kept && (offset < 0 || offset + length > bytes.byteLength)) {
    throw new Error(`${mapName} lump ${i} lies outside the file`);
  }
  // Lumps stay 4-aligned, as compilers write them.
  const start = kept ? (at + 3) & ~3 : 0;
  if (kept) {
    parts.push(new Uint8Array(start - at), bytes.subarray(offset, offset + length));
    at = start + length;
  }
  directory.setInt32(8 * i, start, true);
  directory.setInt32(8 * i + 4, kept ? length : 0, true);
}
const header = Buffer.from(JSON.stringify({ map: mapName, bsp_sha256: sha }), "utf8");
const u32 = (value) => {
  const buffer = Buffer.alloc(4);
  buffer.writeUInt32LE(value);
  return buffer;
};
const trimmed = Buffer.concat([bytes.subarray(0, 8), new Uint8Array(directory.buffer), ...parts]);
await writeFile(output, Buffer.concat([Buffer.from("SHAGCOL1", "ascii"), u32(header.length), header, trimmed]));
console.log(
  `Wrote ${mapName}'s collision, ${trimmed.byteLength} of its ${bytes.byteLength} BSP bytes (sha256 ${sha}), to ${output}`,
);
