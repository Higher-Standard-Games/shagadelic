import assert from "node:assert/strict";
import { copyFile, mkdtemp, rm, rmdir, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { WashoiSupervisor } from "../tools/washoi-supervisor.mjs";

const executable = process.argv[2];
if (!executable) throw new Error("Usage: node tests/washoi-supervisor.test.mjs <shagadelic executable>");

test("match ownership is independent of the endpoint and survives zero bots", { timeout: 10_000 }, async (t) => {
  const supervisor = new WashoiSupervisor(executable);
  t.after(() => supervisor.stopAll());
  const endpoint = "ws://127.0.0.1:1";
  const first = supervisor.start("first", endpoint);
  assert.equal(supervisor.start("first", endpoint), first);
  await Promise.all([first, supervisor.start("second", endpoint)]);
  await assert.rejects(supervisor.launch("first", { name: "test", difficulty: "unknown" }), /unknown_difficulty/);
  const custom = { reactionTimeMs: 200 };
  await assert.rejects(supervisor.launch("first", { name: "test", difficulty: custom }), /preset name/);
  await assert.rejects(supervisor.launch("first", { name: "test" }), /admission_error/);
  assert.equal(supervisor.start("first", endpoint), first);
  await supervisor.stop("first");
  await assert.rejects(supervisor.launch("first", { name: "test" }), /no bot child/);
  await supervisor.start("second", endpoint);
  await supervisor.start("third", endpoint);
});

test("a match ending during child startup rejects readiness and reaps the child", { timeout: 5_000 }, async (t) => {
  const supervisor = new WashoiSupervisor(executable);
  t.after(() => supervisor.stopAll());
  const ready = supervisor.start("short", "ws://127.0.0.1:1");
  const rejected = assert.rejects(ready, /stopped before readiness/);
  await Promise.all([rejected, supervisor.stop("short")]);
  await assert.rejects(supervisor.launch("short", { name: "test" }), /no bot child/);
});

test("map assets use the match map and reject a file labeled for a different map", { timeout: 5_000 }, async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "shagadelic-nav-"));
  const supervisor = new WashoiSupervisor(executable, { navigationDir: directory });
  t.after(async () => {
    await supervisor.stopAll();
    await rm(join(directory, "washoi_dm2.json"), { force: true });
    await rm(join(directory, "washoi_dm2.sight"), { force: true });
    await rm(join(directory, "washoi_dm2.collision"), { force: true });
    await rmdir(directory);
  });
  const endpoint = "ws://127.0.0.1:1";
  await supervisor.start("missing", endpoint, "washoi_dm2");
  await supervisor.stop("missing");
  const file = join(directory, "washoi_dm2.json");
  const route = { entry: [0, 0, 0], waypoints: [[32, 0, 0]] };
  await writeFile(file, JSON.stringify({ map: "other", routes: [route] }));
  await assert.rejects(supervisor.start("wrong", endpoint, "washoi_dm2"));
  // Padding makes a valid file larger than one control record.
  await writeFile(file, JSON.stringify({ map: "washoi_dm2", routes: [route] }) + " ".repeat(70_000));
  await supervisor.start("correct", endpoint, "washoi_dm2");
  const sight = join(directory, "washoi_dm2.sight");
  await writeFile(sight, emptySight("other"));
  await assert.rejects(supervisor.start("wrong sight", endpoint, "washoi_dm2"));
  await writeFile(sight, emptySight("washoi_dm2"));
  await supervisor.start("sight", endpoint, "washoi_dm2");
  // Collision from a BSP the authored routes were not baked from.
  const collision = join(directory, "washoi_dm2.collision");
  await copyFile(new URL("./fixtures/box.collision", import.meta.url), collision);
  await assert.rejects(supervisor.start("foreign collision", endpoint, "washoi_dm2"));
  await rm(collision);
});

/** A sight file with no geometry, in the format of tools/bake-washoi-sight.mjs. */
function emptySight(map) {
  const header = Buffer.from(JSON.stringify({ map, bsp_sha256: "0" }));
  const count = (value) => {
    const buffer = Buffer.alloc(4);
    buffer.writeUInt32LE(value);
    return buffer;
  };
  return Buffer.concat([Buffer.from("SHAGSGT1"), count(header.length), header, count(0), count(0), count(0)]);
}

const endpoint = process.env.SHAGADELIC_TEST_WASHOI;
test("the supervisor disposes bots and reaps the child with a live bot still connected", {
  timeout: 15_000,
  skip: !endpoint && "requires a local Washoi game server",
}, async (t) => {
  const supervisor = new WashoiSupervisor(executable);
  t.after(() => supervisor.stopAll());
  await supervisor.start("live", endpoint, "washoi_dm2");
  const [first, second] = await Promise.all([
    supervisor.launch("live", { name: "SHAGADELIC supervised one", difficulty: "hard" }),
    supervisor.launch("live", { name: "SHAGADELIC supervised two", difficulty: "easy" }),
  ]);
  assert.notEqual(first.playerId, second.playerId);
  assert.equal(await first.dispose(), "removed");
  await supervisor.stop("live");
  assert.equal(await second.ended, "child_exit");
});
