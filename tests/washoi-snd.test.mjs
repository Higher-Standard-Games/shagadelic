import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { createServer } from "node:net";
import { resolve } from "node:path";
import { createInterface } from "node:readline";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";

import { WashoiSupervisor } from "../tools/washoi-supervisor.mjs";

const [executable, shagCheckout, navigationDir] = process.argv.slice(2);
if (!executable || !shagCheckout || !navigationDir) {
  throw new Error("Usage: node tests/washoi-snd.test.mjs <executable> <SHAG checkout> <navigation directory>");
}

test("four Washoi bots can collect and plant the bomb in S&D", { timeout: 115_000 }, async (t) => {
  const reservation = createServer().listen(0, "127.0.0.1");
  await once(reservation, "listening");
  const { port } = reservation.address();
  reservation.close();
  await once(reservation, "close");

  const fixture = fileURLToPath(new URL("./fixtures/washoi-snd-server.ts", import.meta.url));
  const server = spawn(process.execPath, ["node_modules/vite-node/vite-node.mjs", fixture], {
    cwd: resolve(shagCheckout),
    env: { ...process.env, SHAGADELIC_TEST_PORT: String(port) },
    stdio: ["ignore", "pipe", "inherit"],
    windowsHide: true,
  });
  const supervisor = new WashoiSupervisor(resolve(executable), { navigationDir });
  t.after(async () => {
    try {
      await supervisor.stopAll();
    } finally {
      if (server.pid && server.exitCode === null && server.signalCode === null) {
        const reaped = once(server, "close");
        server.kill();
        await reaped;
      }
    }
  });

  const ready = Promise.withResolvers();
  const planted = Promise.withResolvers();
  const closed = new Promise((_, reject) => {
    server.once("error", reject);
    server.once("exit", (code) => reject(new Error(`Washoi server exited with code ${code}`)));
  });
  createInterface({ input: server.stdout }).on("line", (line) => {
    if (line === "Ready for connections") ready.resolve();
    if (line === "SHAGADELIC plant confirmed") planted.resolve();
  });
  await waitFor(ready.promise, closed, 15_000, "Washoi server readiness");
  await supervisor.start("snd-plant", `ws://127.0.0.1:${port}`, "washoi_snd1");
  const identities = new Set();
  for (const [index, team] of ["soba", "udon", "soba", "udon"].entries()) {
    const bot = await supervisor.launch("snd-plant", { name: `SHAGADELIC ${index}`, difficulty: "medium", team });
    identities.add(bot.playerId);
  }
  assert.equal(identities.size, 4);
  await waitFor(planted.promise, closed, 80_000, "server-confirmed bomb plant");
});

function waitFor(result, closed, ms, label) {
  return Promise.race([
    result, closed,
    delay(ms, undefined, { ref: false }).then(() => { throw new Error(`Timed out waiting for ${label}`); }),
  ]);
}
