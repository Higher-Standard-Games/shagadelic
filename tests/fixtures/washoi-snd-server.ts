// Run under vite-node from the assembled SHAG checkout.
import { GameServer } from "/src/games/fps/server/GameServer";
import { loadBspCollision } from "/src/games/fps/cli/HeadlessMapLoader";
import { SearchAndDestroy } from "/src/games/fps/server/gametypes/SearchAndDestroy";

const map = "washoi_snd1";
const data = await loadBspCollision(map);
if (!data) throw new Error("missing indexed map");

const mode = new SearchAndDestroy();
const server = new GameServer({
  port: Number(process.env.SHAGADELIC_TEST_PORT),
  serverName: "SHAGADELIC S&D fixture",
  mapName: map, maxClients: 8, tickRate: 125, snapshotRate: 62.5, certPath: "", keyPath: "",
}, mode);
server.setCollisionWorld(data.collision);
server.setSpawnPoints(data.spawnPoints, data.teamSpawnPoints);
server.setMapData(data.entities, data.models);
server.setPickupItems(data.pickups);
server.setTriggerVolumes(data.triggers);
server.startRecording = () => {};
await server.start();
console.log("Ready for connections");

const controller = server.getGametypeController();
const waitForBots = setInterval(() => {
  if ([...server.clients.values()].filter((client) => client.entityId).length < 4) return;
  clearInterval(waitForBots);
  controller.getMatchState().expireWarmup();
}, 250);

const waitForPlant = setInterval(() => {
  if (mode.getRoundState() !== "post_plant") return;
  clearInterval(waitForPlant);
  console.log("SHAGADELIC plant confirmed");
}, 100);
