import { GameServer } from "/src/games/fps/server/GameServer";
import { loadBspCollision } from "/src/games/fps/cli/HeadlessMapLoader";
import { Deathmatch } from "/src/games/fps/server/gametypes/Deathmatch";

const map = "washoi_dm2";
const data = await loadBspCollision(map, "./game");
if (!data) throw new Error("missing indexed map");
const mode = new Deathmatch();
const server = new GameServer({
  port: Number(process.env.SHAGADELIC_TEST_PORT),
  serverName: "SHAGADELIC traversal fixture",
  mapName: map, maxClients: 2, tickRate: 125, snapshotRate: 62.5, certPath: "", keyPath: "",
}, mode);
server.setCollisionWorld(data.collision);
server.setSpawnPoints([JSON.parse(process.env.SHAGADELIC_TEST_TRAVERSAL_SPAWN)], new Map());
server.gametypeController.jitterSpawn = (start) => start;
server.setMapData(data.entities, data.models);
server.setTriggerVolumes(data.triggers);
await server.start();
console.log("Ready for connections");
