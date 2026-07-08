// OpenWire Signaling — standalone server
// Usage: node server.js [port]
// No Cloudflare, no domain, no dependencies.
// Clients connect via `ws://1.2.3.4:port/api/signal/:room`

const http = require("http");

const PORT = parseInt(process.argv[2]) || 8080;

// ── in-memory store ───────────────────────────────────────
const relays = new Map();   // id => { id, addr, lastSeen }
const rooms = new Map();    // room => Set<ws>

// ── HTTP router ───────────────────────────────────────────
const server = http.createServer((req, res) => {
  res.setHeader("Access-Control-Allow-Origin", "*");
  let url = new URL(req.url, `http://${req.headers.host}`);
  let [_, part1, part2, part3] = url.pathname.split("/");

  if (!part1) return json(res, { ok: true, service: "openwire-server" });

  // GET /api/relays
  if (part1 === "api" && part2 === "relays" && req.method === "GET") {
    let now = Date.now();
    let active = [];
    for (let [id, r] of relays) {
      if (now - r.lastSeen > 120_000) { relays.delete(id); continue; }
      active.push([r.id, r.addr]);
    }
    return json(res, { ok: true, relays: active, ttl_secs: 120 });
  }

  // POST /api/relays
  if (part1 === "api" && part2 === "relays" && req.method === "POST") {
    return readJson(req, body => {
      if (!body.id || !body.addr) return json(res, { ok: false, error: "missing id or addr" }, 400);
      relays.set(body.id, { id: body.id, addr: body.addr, lastSeen: Date.now() });
      json(res, { ok: true, relay: { id: body.id, addr: body.addr, status: "registered" } });
    });
  }

  // POST /api/relays/ping
  if (part1 === "api" && part2 === "relays" && part3 === "ping") {
    return readJson(req, body => {
      let r = relays.get(body.id);
      if (!r) return json(res, { ok: false, error: "unknown relay" }, 404);
      r.lastSeen = Date.now();
      json(res, { ok: true, relay: { id: r.id, status: "refreshed" } });
    });
  }

  // GET /api/nodes.json
  if (part1 === "api" && part2 === "nodes.json") {
    return json(res, { relay_nodes: [...relays.values()].map(r => [r.id, r.addr]), bootstrap_nodes: [] });
  }

  return json(res, { ok: false, error: "not found" }, 404);
});

// ── WebSocket signaling ───────────────────────────────────
const { WebSocketServer } = require("ws");

const wss = new WebSocketServer({ server, path: undefined });

wss.on("connection", (ws, req) => {
  let url = new URL(req.url, `http://${req.headers.host}`);
  let [_, part1, part2, room] = url.pathname.split("/");

  if (part1 !== "api" || part2 !== "signal" || !room) {
    ws.close(4000, "invalid path");
    return;
  }

  if (!rooms.has(room)) rooms.set(room, new Set());
  let peers = rooms.get(room);
  let info = { peerId: null, addrs: [] };
  peers.add(ws);

  ws.on("message", data => {
    try {
      let msg = JSON.parse(data);
      switch (msg.type) {
        case "register":
          info.peerId = msg.peer_id;
          info.addrs = msg.addrs || [];
          // tell others
          for (let p of peers) {
            if (p !== ws && p.readyState === 1) {
              p.send(JSON.stringify({ type: "peer", peer_id: info.peerId, addrs: info.addrs }));
            }
          }
          // tell new peer about existing ones
          for (let p of peers) {
            if (p !== ws && p._info && p._info.peerId) {
              ws.send(JSON.stringify({ type: "peer", peer_id: p._info.peerId, addrs: p._info.addrs }));
            }
          }
          ws._info = info;
          console.log(`register room=${room} peer=${info.peerId?.slice(0,16)} addrs=${info.addrs.length}`);
          break;

        case "signal":
          for (let p of peers) {
            if (p !== ws && p._info && p._info.peerId === msg.target) {
              p.send(JSON.stringify({ type: "signal", from: info.peerId, data: msg.data }));
              break;
            }
          }
          break;
      }
    } catch (e) { /* ignore bad json */ }
  });

  ws.on("close", () => {
    peers.delete(ws);
    if (peers.size === 0) rooms.delete(room);
    if (info.peerId) {
      for (let p of peers) {
        if (p.readyState === 1) p.send(JSON.stringify({ type: "peer_left", peer_id: info.peerId }));
      }
    }
  });
});

server.listen(PORT, "0.0.0.0", () => {
  console.log(`OpenWire signaling server running on port ${PORT}`);
  console.log(`HTTP:  http://<your-ip>:${PORT}/api/relays`);
  console.log(`WS:    ws://<your-ip>:${PORT}/api/signal/:room`);
});

// ── helpers ───────────────────────────────────────────────

function json(res, data, status = 200) {
  res.writeHead(status, { "content-type": "application/json" });
  res.end(JSON.stringify(data));
}

function readJson(req, cb) {
  let buf = [];
  req.on("data", c => buf.push(c));
  req.on("end", () => { try { cb(JSON.parse(Buffer.concat(buf))); } catch (_) { cb({}); } });
}