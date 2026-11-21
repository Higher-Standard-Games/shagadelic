import { GameServer } from "/src/games/fps/server/GameServer";
import { loadBspCollision } from "/src/games/fps/cli/HeadlessMapLoader";
import { Deathmatch } from "/src/games/fps/server/gametypes/Deathmatch";

const map = "washoi_dm2";
const data = await loadBspCollision(map, "./game");
if (!data) throw new Error("missing indexed map");
const mode = new Deathmatch();
mode.getAgentDirectives = ({tick, alive}) => alive ? [{
  id: "hold_dm2",
  kind: "hold_area",
  target: {type: "position", position: {x: -88, y: 192, z: 312}, radius: 32},
  priority: 1,
  expiresAtTick: tick + 125,
  interrupt: "never",
  via: [{x: -232, y: 192, z: 192}],
  watch: {x: -232, y: 192, z: 192},
}] : [];
const server = new GameServer({
  port: Number(process.env.SHAGADELIC_TEST_PORT),
  serverName: "SHAGADELIC hold fixture",
  mapName: map, maxClients: 2, tickRate: 125, snapshotRate: 62.5, certPath: "", keyPath: "",
}, mode);
server.setCollisionWorld(data.collision);
server.setSpawnPoints(data.spawnPoints.slice(0, 1), new Map());
server.gametypeController.jitterSpawn = (start) => start;
server.setMapData(data.entities, data.models);
await server.start();
console.log("Ready for connections");
