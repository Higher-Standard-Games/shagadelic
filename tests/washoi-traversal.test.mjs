import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { mkdtemp, readFile, rm, rmdir, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { createInterface } from "node:readline";
import { setTimeout as delay } from "node:timers/promises";

const [checkout, baked, variant] = process.argv.slice(2);
const fixtures = {
  jump: { edge: [4, 32] },
  jumppad: { edge: [28, 0], spawn: [16, 24, -816] },
  teleporter: { edge: [29, 30], spawn: [-382, 169, 438] },
};
if (!checkout || !baked || !Object.hasOwn(fixtures, variant)) {
  throw new Error("Usage: node tests/washoi-traversal.test.mjs <SHAG checkout> <baked DM2 graph> <jump|jumppad|teleporter>");
}
const source = JSON.parse(await readFile(baked, "utf8"));
assert.equal(source.map, "washoi_dm2");
const { bsp_sha256, nodes, maneuvers } = source.graph;
const { edge, spawn: start } = fixtures[variant];
const selected = maneuvers.find(([from, to]) => edge[0] === from && edge[1] === to);
assert.ok(selected, `No DM2 ${variant} fixture edge in the baked graph`);
const [from, to, action] = selected;
assert.equal(action.kind, variant);
const approach = start ?? nodes[from];
const [x, y, z] = approach;
const directory = await mkdtemp(join(tmpdir(), "washoi-traversal-"));
const graph = join(directory, "graph.json");
let server;
try {
  await writeFile(graph, JSON.stringify({
    map: source.map,
    graph: {
      bsp_sha256, nodes: [approach, nodes[from], nodes[to]], entries: [0], goals: [2],
      edges: [[0, 1]], maneuvers: [[1, 2, action]],
    },
  }));
  const reservation = createServer().listen(0, "127.0.0.1");
  await once(reservation, "listening");
  const { port } = reservation.address();
  const released = once(reservation, "close");
  reservation.close();
  await released;
  const fixture = resolve("tests/fixtures/washoi-traversal-server.ts");
  server = spawn(process.execPath, ["node_modules/vite-node/vite-node.mjs", fixture], {
    cwd: resolve(checkout), stdio: ["ignore", "pipe", "inherit"], windowsHide: true,
    env: {
      ...process.env,
      SHAGADELIC_TEST_PORT: String(port),
      SHAGADELIC_TEST_TRAVERSAL_SPAWN: JSON.stringify({ x, y, z }),
    },
  });
  const ready = new Promise((resolve, reject) => {
    server.once("error", reject);
    server.once("exit", (code) => reject(new Error(`Washoi fixture exited with status ${code}`)));
    createInterface({ input: server.stdout }).on("line", (line) => {
      if (line === "Ready for connections") resolve();
    });
  });
  await Promise.race([
    ready,
    delay(10_000, undefined, { ref: false }).then(() => { throw new Error("Washoi fixture startup timed out"); }),
  ]);
  const test = spawn("cargo", ["test", "--features", "washoi", "--test", "washoi_traversal", "--", "--ignored"], {
    stdio: "inherit",
    env: {
      ...process.env,
      SHAGADELIC_TEST_WASHOI: `ws://127.0.0.1:${port}`,
      SHAGADELIC_TEST_TRAVERSAL_GRAPH: graph,
      SHAGADELIC_TEST_TRAVERSAL_KIND: variant,
      SHAGADELIC_TEST_TRAVERSAL_TARGET: JSON.stringify(action.exit ?? action.landing),
    },
  });
  assert.deepEqual(await once(test, "exit"), [0, null]);
} finally {
  if (server?.pid && server.exitCode === null && server.signalCode === null) {
    const reaped = once(server, "close");
    server.kill();
    await reaped;
  }
  await rm(graph, { force: true });
  await rmdir(directory);
}
