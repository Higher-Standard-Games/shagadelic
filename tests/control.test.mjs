import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { createInterface } from "node:readline";
import { setTimeout as delay } from "node:timers/promises";
import { test } from "node:test";

const executable = process.argv[2];
if (!executable) {
  throw new Error("Usage: node tests/control.test.mjs <shagadelic executable>");
}

const options = { timeout: 5_000 };
const initialization = { type: "initialize", version: 1, lobby_id: "test-lobby" };
const ready = { ...initialization, type: "ready" };

function write(child, command) {
  child.stdin.write(JSON.stringify(command) + "\n");
}

function start(t, command, args, stdio) {
  const child = spawn(command, args, { stdio, windowsHide: true });
  const closed = once(child, "close");
  const lines = createInterface({ input: child.stdout });
  t.after(() => {
    lines.close();
    if (child.exitCode === null && child.signalCode === null) child.kill();
  });
  return { child, closed, lines };
}

test("an empty lobby stays ready and shuts down on request", options, async (t) => {
  const { child, closed, lines } = start(t, executable, [], ["pipe", "pipe", "inherit"]);
  const reply = once(lines, "line");
  // Split the initialization across writes to exercise stream framing.
  const record = JSON.stringify(initialization);
  child.stdin.write(record.slice(0, 8));
  child.stdin.write(record.slice(8) + "\n");
  assert.deepEqual(JSON.parse((await reply)[0]), ready);

  await delay(100);
  assert.equal(child.exitCode, null);
  assert.equal(child.signalCode, null);
  write(child, { type: "shutdown" });
  assert.deepEqual(await closed, [0, null]);
});

test("an unsupported version exits before readiness", options, async (t) => {
  const { child, closed, lines } = start(t, executable, [], ["pipe", "pipe", "pipe"]);
  let diagnostic = "";
  child.stderr.setEncoding("utf8").on("data", (chunk) => {
    diagnostic += chunk;
  });
  const output = [];
  lines.on("line", (line) => output.push(line));
  write(child, { ...initialization, version: 2 });
  const [code, signal] = await closed;
  assert.equal(signal, null);
  assert.equal(code, 1);
  assert.deepEqual(output, []);
  assert.match(diagnostic, /unsupported control version/);
});

test("the lobby exits after its host is killed", options, async (t) => {
  // The Rust child inherits the host's stdout pipe. The outer process sees
  // EOF only once both processes have closed their copy of the pipe.
  const hostScript = `
    const { spawn } = require("node:child_process");
    const child = spawn(process.argv[1], [], {
      stdio: ["pipe", "inherit", "inherit"], windowsHide: true,
    });
    child.stdin.write(process.argv[2] + "\\n");
  `;
  const { child: host, closed, lines } = start(
    t, process.execPath,
    ["-e", hostScript, executable, JSON.stringify(initialization)],
    ["ignore", "pipe", "inherit"],
  );
  assert.deepEqual(JSON.parse((await once(lines, "line"))[0]), ready);
  const outputEnded = once(host.stdout, "end");
  assert.ok(host.kill("SIGKILL"));
  assert.deepEqual(await closed, [null, "SIGKILL"]);
  await outputEnded;
});
