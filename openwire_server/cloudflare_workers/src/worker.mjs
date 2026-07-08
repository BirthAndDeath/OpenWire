// =============================================================================
// OpenWire Server — Relay Registry + Bootstrap + Presence + Signaling
//
// Uses Durable Objects for coordination:
//   - RelayRegistry: public relay nodes heartbeat their address here
//   - Presence:     name ↔ mldsa-pubkey mapping for friend discovery
//   - SignalingRoom: WebSocket signaling exchange for NAT traversal
// =============================================================================

import { RelayRegistry } from "./registry.mjs";
import { Presence } from "./presence.mjs";
import { SignalingRoom } from "./signaling.mjs";
export { RelayRegistry, Presence, SignalingRoom };

async function json(data, status = 200) {
  return new Response(JSON.stringify(data), {
    status,
    headers: { "content-type": "application/json", "access-control-allow-origin": "*" },
  });
}

function error(msg, status = 400) {
  return json({ ok: false, error: msg }, status);
}

function log(method, path, info) {
  console.log(`${method} ${path} — ${info}`);
}

const RELAY_TTL = 120;

// Bootstrap 节点列表。部署前请替换为你的实际 bootstrap 节点地址。
// 格式: [["<PeerId>", "<Multiaddr>"], ...]
const DEFAULT_BOOTSTRAP = [];

export default {
  async fetch(request, env) {
    let url = new URL(request.url);
    let [part1, part2, part3] = url.pathname.slice(1).split("/");

    try {
      if (!part1) {
        return json({ ok: true, service: "openwire-server", version: "0.1.0", docs: "/api" });
      }

      if (part1 === "api" && !part2) {
        return json({
          ok: true,
          endpoints: {
            "GET  /api/nodes.json":           "full nodes config (relays + bootstrap) for direct file drop-in",
            "GET  /api/bootstrap":            "list bootstrap nodes",
            "GET  /api/relays":               "list active relay nodes",
            "POST /api/relays":               "register as relay (body: {id, addr})",
            "POST /api/relays/ping":          "relay heartbeat (body: {id})",
            "WS   /api/signal/:room":        "WebSocket signaling room for NAT traversal",
            "GET  /api/presence/:name":       "resolve name → pubkey",
            "PUT  /api/presence/:name":       "set name → pubkey (body: {pubkey})",
            "DELETE /api/presence/:name":     "remove mapping",
          },
        });
      }

      if (part1 === "api" && part2 === "nodes.json") {
        return await handleNodesJson(request, env);
      }

      if (part1 === "api" && part2 === "relays" && part3 === "ping") {
        return await handleRelayPing(request, env);
      }
      if (part1 === "api" && part2 === "relays") {
        return await handleRelays(request, env);
      }

      if (part1 === "api" && part2 === "bootstrap") {
        return json({ ok: true, nodes: DEFAULT_BOOTSTRAP });
      }

      if (part1 === "api" && part2 === "presence" && part3) {
        return await handlePresence(request, env, part3);
      }

      // ---- Signaling Room (WebSocket) ----
      if (part1 === "api" && part2 === "signal" && part3) {
        log("WS", `/api/signal/${part3}`, "upgrade");
        return await handleSignal(request, env, part3);
      }

      return error("not found", 404);
    } catch (err) {
      return error(err.message, 500);
    }
  },
};

async function handleNodesJson(request, env) {
  let id = env.RELAY_REGISTRY.idFromName("global");
  let stub = env.RELAY_REGISTRY.get(id);
  let resp = await stub.fetch("https://dummy/list");
  let relays = await resp.json();
  return json({ relay_nodes: relays, bootstrap_nodes: DEFAULT_BOOTSTRAP });
}

async function handleRelays(request, env) {
  if (request.method === "GET") {
    let id = env.RELAY_REGISTRY.idFromName("global");
    let stub = env.RELAY_REGISTRY.get(id);
    let resp = await stub.fetch("https://dummy/list");
    let relays = await resp.json();
    return json({ ok: true, relays, ttl_secs: RELAY_TTL });
  }

  if (request.method === "POST") {
    let body = await request.json();
    if (!body.id || !body.addr) return error("missing required fields: id, addr");
    if (body.addr.length > 512) return error("addr too long");
    let id = env.RELAY_REGISTRY.idFromName("global");
    let stub = env.RELAY_REGISTRY.get(id);
    let result = await stub.fetch("https://dummy/register", {
      method: "POST",
      body: JSON.stringify({ id: body.id, addr: body.addr }),
    });
    let data = await result.json();
    return json({ ok: result.ok, relay: data, ttl_secs: RELAY_TTL }, result.ok ? 200 : 400);
  }
  return error("method not allowed", 405);
}

async function handleRelayPing(request, env) {
  if (request.method !== "POST") return error("method not allowed", 405);
  let body = await request.json();
  if (!body.id) return error("missing id");
  let id = env.RELAY_REGISTRY.idFromName("global");
  let stub = env.RELAY_REGISTRY.get(id);
  let resp = await stub.fetch("https://dummy/ping", {
    method: "POST",
    body: JSON.stringify({ id: body.id }),
  });
  let data = await resp.json();
  return json({ ok: resp.ok, relay: data }, resp.ok ? 200 : 404);
}

async function handlePresence(request, env, name) {
  if (!name || name.length > 64 || !/^[a-zA-Z0-9_-]+$/.test(name)) {
    return error("invalid name (use alphanumeric, hyphen, underscore)", 400);
  }
  let id = env.PRESENCE.idFromName(name);
  let stub = env.PRESENCE.get(id);

  switch (request.method) {
    case "GET": {
      let resp = await stub.fetch("https://dummy/get");
      let data = await resp.json();
      return json({ ok: true, name, pubkey: data.pubkey || null, signature: data.signature || null });
    }
    case "PUT": {
      let body = await request.json();
      if (!body.pubkey) return error("missing pubkey");
      if (!/^[0-9a-f]+$/i.test(body.pubkey) || body.pubkey.length > 4096) {
        return error("invalid pubkey (expect hex, max 4096 chars)");
      }
      await stub.fetch("https://dummy/set", {
        method: "PUT",
        body: JSON.stringify({ pubkey: body.pubkey, signature: body.signature || null }),
      });
      return json({ ok: true });
    }
    case "DELETE": {
      await stub.fetch("https://dummy/delete", { method: "DELETE" });
      return json({ ok: true });
    }
    default:
      return error("method not allowed", 405);
  }
}

async function handleSignal(request, env, room) {
  if (room.length > 64) return error("room name too long", 400);
  let id = env.SIGNALING_ROOM.idFromName(room);
  let stub = env.SIGNALING_ROOM.get(id);
  return stub.fetch(request);
}