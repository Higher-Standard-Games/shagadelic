import assert from "node:assert/strict";
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

const endpoint = process.env.SHAGADELIC_TEST_WASHOI;
test("the supervisor disposes bots and reaps the child with a live bot still connected", {
  timeout: 15_000,
  skip: !endpoint && "requires a local Washoi game server",
}, async (t) => {
  const supervisor = new WashoiSupervisor(executable);
  t.after(() => supervisor.stopAll());
  await supervisor.start("live", endpoint);
  const [first, second] = await Promise.all([
    supervisor.launch("live", { name: "SHAGADELIC supervised one" }),
    supervisor.launch("live", { name: "SHAGADELIC supervised two" }),
  ]);
  assert.notEqual(first.playerId, second.playerId);
  assert.equal(await first.dispose(), "removed");
  await supervisor.stop("live");
  assert.equal(await second.ended, "child_exit");
});
