import { spawn } from "node:child_process";
import { access } from "node:fs/promises";
import { resolve } from "node:path";
import { createInterface } from "node:readline";

const STARTUP_MS = 5_000;
const ADMISSION_MS = 12_000;
const SHUTDOWN_MS = 2_000;
const MAX_BOTS = 64;
const MAX_RECORD_BYTES = 64 * 1024;

/** Owns one Rust child for each Washoi match. Call start after the game server is ready. */
export class WashoiSupervisor {
  #executable;
  #matches = new Map();
  #assets;

  constructor(executable, {
    navigationDir = process.env.SHAGADELIC_NAVIGATION_DIR,
    learnedRoutesDir = process.env.SHAGADELIC_LEARNED_ROUTES_DIR,
  } = {}) {
    this.#executable = executable;
    this.#assets = { navigationDir, learnedRoutesDir };
  }

  start(matchId, endpoint, map) {
    const existing = this.#matches.get(matchId);
    if (existing) {
      if (existing.endpoint !== endpoint || existing.map !== map || existing.closing) {
        return Promise.reject(new Error("match already has a different or closing bot child"));
      }
      return existing.ready;
    }
    const match = new MatchChild(this.#executable, matchId, endpoint, map, this.#assets);
    this.#matches.set(matchId, match);
    void match.closed.then(() => {
      if (this.#matches.get(matchId) === match) this.#matches.delete(matchId);
    });
    return match.ready;
  }

  async launch(matchId, options) {
    const match = this.#matches.get(matchId);
    if (!match) throw new Error("match has no bot child");
    await match.ready;
    return match.launch(options);
  }

  async stop(matchId) {
    await this.#matches.get(matchId)?.stop();
  }

  async stopAll() {
    await Promise.all([...this.#matches.values()].map((match) => match.stop()));
  }
}

class MatchChild {
  #child;
  #ready = Promise.withResolvers();
  #closed = Promise.withResolvers();
  #bots = new Map();
  #nextId = 0;
  #started = false;
  #initialized = false;
  #closing = false;
  #startupTimer;
  #shutdownTimer;

  constructor(executable, matchId, endpoint, map, assets) {
    this.endpoint = endpoint;
    this.map = map;
    this.#child = spawn(executable, [], { stdio: ["pipe", "pipe", "inherit"], windowsHide: true });
    const output = createInterface({ input: this.#child.stdout });
    output.on("line", (line) => {
      try {
        if (Buffer.byteLength(line) >= MAX_RECORD_BYTES) throw new Error();
        const event = JSON.parse(line);
        if (event.type === "ready" && !this.#started) {
          if (event.version !== 1 || event.lobby_id !== matchId) throw new Error();
          this.#started = true;
          clearTimeout(this.#startupTimer);
          this.#ready.resolve();
        } else if (this.#started) {
          this.#receive(event);
        } else {
          throw new Error();
        }
      } catch {
        this.#fail("invalid bot child control record");
      }
    });
    this.#child.once("error", () => this.#fail("could not start or control bot child"));
    this.#child.stdin.on("error", () => this.#fail("bot child control pipe closed"));
    this.#child.once("close", () => {
      output.close();
      clearTimeout(this.#startupTimer);
      clearTimeout(this.#shutdownTimer);
      this.#closing = true;
      this.#ready.reject(new Error("bot child exited before readiness"));
      for (const id of this.#bots.keys()) this.#finish(id, "child_exit");
      this.#closed.resolve();
    });
    this.#startupTimer = setTimeout(() => this.#fail("bot child readiness timed out"), STARTUP_MS);
    void this.#initialize(matchId, endpoint, map, assets);
  }

  get ready() { return this.#ready.promise; }
  get closed() { return this.#closed.promise; }
  get closing() { return this.#closing; }

  launch({ name, ticket, team, difficulty }) {
    if (this.#closing) throw new Error("bot child is closing");
    if (this.#bots.size >= MAX_BOTS) throw new Error("bot child capacity reached");
    // A malformed record ends the whole child, so reject custom BotDifficulty
    // objects here. The child reports unknown preset names per request.
    if (difficulty !== undefined && typeof difficulty !== "string") {
      throw new Error("bot difficulty must be a preset name");
    }
    const id = `bot-${++this.#nextId}`;
    const admission = Promise.withResolvers();
    const ended = Promise.withResolvers();
    const bot = {
      admission, ended, generation: undefined, removing: false,
      timer: setTimeout(() => {
        admission.reject(new Error("bot admission timed out"));
        this.#remove(id, bot);
      }, ADMISSION_MS),
    };
    this.#bots.set(id, bot);
    if (!this.#write({ type: "spawn", id, name, ticket, team, difficulty })) this.#finish(id, "control_write_failed");
    return admission.promise;
  }

  stop() {
    if (!this.#closing) {
      this.#closing = true;
      clearTimeout(this.#startupTimer);
      if (!this.#started) this.#ready.reject(new Error("bot child stopped before readiness"));
      if (this.#initialized) this.#write({ type: "shutdown" });
      this.#child.stdin.end();
      this.#shutdownTimer = setTimeout(() => this.#child.kill("SIGKILL"), SHUTDOWN_MS);
    }
    return this.closed;
  }

  async #initialize(matchId, endpoint, map, assets) {
    try {
      const options = await mapAssets(map, assets);
      if (this.#closing) return;
      this.#initialized = this.#write({
        type: "initialize", version: 1, lobby_id: matchId, washoi_endpoint: endpoint, ...options,
      });
      if (!this.#initialized) this.#fail("bot child initialization could not be sent");
    } catch (error) {
      this.#fail(`could not load bot map assets: ${error.message}`);
    }
  }

  #write(record) {
    const line = JSON.stringify(record) + "\n";
    if (!this.#child.stdin.writable || Buffer.byteLength(line) > MAX_RECORD_BYTES) return false;
    this.#child.stdin.write(line);
    return true;
  }

  #receive(event) {
    const bot = this.#bots.get(event.id);
    if (!bot) return;
    if (event.type === "bot_status") {
      if (!Number.isSafeInteger(event.generation) || event.generation <= 0) throw new Error();
      if (bot.generation !== undefined && bot.generation !== event.generation) throw new Error();
      bot.generation = event.generation;
      if (bot.removing) this.#remove(event.id, bot);
    } else if (event.type === "bot_request_failed") {
      this.#finish(event.id, typeof event.reason === "string" ? event.reason : "request_failed");
    } else if (event.generation === bot.generation && bot.generation !== undefined) {
      if (event.type === "bot_admitted" && !bot.removing) {
        if (typeof event.player_id !== "string" || !event.player_id) throw new Error();
        clearTimeout(bot.timer);
        bot.admission.resolve({
          id: event.id, playerId: event.player_id, ended: bot.ended.promise,
          dispose: () => {
            if (this.#bots.get(event.id) === bot && !bot.removing) this.#remove(event.id, bot);
            return bot.ended.promise;
          },
        });
      } else if (event.type === "bot_ended") {
        this.#finish(event.id, typeof event.reason === "string" ? event.reason : "ended");
      }
    }
  }

  #remove(id, bot) {
    bot.removing = true;
    if (bot.generation === undefined || this.#closing) return;
    if (!this.#write({ type: "remove", id, generation: bot.generation })) this.#fail("bot removal could not be sent");
    clearTimeout(bot.timer);
    bot.timer = setTimeout(() => this.#fail("bot removal timed out"), SHUTDOWN_MS);
  }

  #finish(id, reason) {
    const bot = this.#bots.get(id);
    if (!bot) return;
    this.#bots.delete(id);
    clearTimeout(bot.timer);
    bot.admission.reject(new Error(`bot ended before admission (${reason})`));
    bot.ended.resolve(reason);
  }

  #fail(reason) {
    if (this.#closing) return;
    this.#closing = true;
    clearTimeout(this.#startupTimer);
    this.#ready.reject(new Error(reason));
    this.#child.kill("SIGKILL");
  }
}

async function mapAssets(map, { navigationDir, learnedRoutesDir }) {
  if (!map || (!navigationDir && !learnedRoutesDir)) return {};
  if (!/^[A-Za-z0-9_-]+$/.test(map)) throw new Error("invalid Washoi map name");
  const name = map + ".json";
  const existing = async (path) => {
    await access(path);
    return path;
  };
  const [navigation_file, learned_routes_file, sight_file, locations_file] = await Promise.all([
    navigationDir ? optionalFile(resolve(navigationDir, name), existing) : undefined,
    learnedRoutesDir ? optionalFile(resolve(learnedRoutesDir, name), existing) : undefined,
    // Baked by tools/bake-washoi-sight.mjs. The child checks its map.
    navigationDir ? optionalFile(resolve(navigationDir, map + ".sight"), existing) : undefined,
    // Baked by tools/bake-washoi-locations.mjs, for team callouts.
    navigationDir ? optionalFile(resolve(navigationDir, map + ".locations.json"), existing) : undefined,
  ]);
  return { washoi_map: map, navigation_file, learned_routes_file, sight_file, locations_file };
}

async function optionalFile(path, use) {
  try {
    return await use(path);
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
    return undefined;
  }
}
