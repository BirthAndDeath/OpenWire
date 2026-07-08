# OpenWire Server — Cloudflare Workers

[English](README.md) | [中文](README.zh.md)

A lightweight coordination service for the [OpenWire](https://github.com/BirthAndDeath/OpenWire) P2P chat application, running entirely on Cloudflare's edge network.

## What it provides

| Service | Description | Free Plan |
|---------|-------------|-----------|
| **Relay Registry** | Public relay nodes register via heartbeat; NAT-ed clients discover relays | ✅ |
| **Bootstrap Discovery** | Canonical bootstrap node list for initial DHT join | ✅ |
| **Name → Pubkey Presence** | Human-readable name to ML-DSA public key mapping | ✅ |
| **WebSocket Signaling** | Room-based PeerId + multiaddr exchange for direct NAT traversal | ✅ |
| **nodes.json** | `GET /api/nodes.json` matches `NodesConfig` schema, drop-in for `data_dir/` | ✅ |

## Quick deploy

```bash
npx wrangler login
cd openwire_server/cloudflare_workers
npx wrangler deploy
```

Then enable Durable Objects in the Cloudflare dashboard under Workers → openwire-server → Durable Objects.

## How OpenWire uses this

### Bootstrap + Relays (manual)

Fetch once and write to `data_dir/nodes.json`:

```bash
curl -s https://your-worker.example/api/nodes.json > /path/to/data_dir/nodes.json
```

OpenWire reads this on next startup via `CoreConfig::load_nodes_config()` → `NodesConfig::load()`.

### Presence (name → pubkey)

```bash
# Register
curl -X PUT https://your-worker.example/api/presence/alice \
  -H 'Content-Type: application/json' \
  -d '{"pubkey":"<mldsa_public_key_hex>"}'

# Lookup
curl https://your-worker.example/api/presence/alice

# Delete
curl -X DELETE https://your-worker.example/api/presence/alice
```

### Signaling (WebSocket NAT traversal)

Configured via `CoreConfig` — no manual WS handling needed:

```rust
cfg.signaling_server = Some("your-worker.example".into());
cfg.signaling_room = Some("my-room".into());  // optional, defaults to first 16 hex chars of pubkey
```

On startup, `ChatCore::try_init()` creates a `SignalingActor` that:

1. Connects to `wss://your-worker.example/api/signal/my-room`
2. Sends `{"type":"register","peer_id":"...","addrs":["...","..."]}`
3. Receives `{"type":"peer","peer_id":"...","addrs":["...","..."]}` from other peers
4. Auto-dials each discovered address via `P2pCommand::DialAddr`

### Signaling protocol (JSON over WebSocket)

| Direction | Message | Description |
|-----------|---------|-------------|
| Client → | `{"type":"register","peer_id":"...","addrs":["..."]}` | Announce presence and addresses |
| Server → | `{"type":"peer","peer_id":"...","addrs":["..."]}` | Another peer joined |
| Server → | `{"type":"peer_left","peer_id":"..."}` | A peer disconnected |
| Client ↔ | `{"type":"signal","target":"...","data":"..."}` | Direct signaling to specific peer |

No data relay — only address exchange. The actual libp2p connection (noise handshake + yamux) happens directly between peers.

## Architecture

- **Durable Objects**: one singleton per relay registry, one per name, one per signaling room (DO is created on-demand per room name).
- **Heartbeat eviction**: relays ping every ~60s; stale entries (120s+) auto-removed on read.
- **Zero npm dependencies**: pure Workers runtime API, no package installs.
- **Relay nodes need a VPS**: this service only provides *discovery*, not the relay protocol. Deploy a dedicated OpenWire relay on a VPS and register it via `POST /api/relays`.

## API reference

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/` | Server info |
| `GET` | `/api` | API index |
| `GET` | `/api/nodes.json` | Full nodes config (relays + bootstrap) |
| `GET` | `/api/bootstrap` | List bootstrap nodes |
| `GET` | `/api/relays` | List active relay nodes |
| `POST` | `/api/relays` | Register as relay (`{"id":"..","addr":".."}`) |
| `POST` | `/api/relays/ping` | Relay heartbeat (`{"id":".."}`) |
| `WS` | `/api/signal/:room` | WebSocket signaling room (NAT traversal) |
| `GET` | `/api/presence/:name` | Resolve name → pubkey |
| `PUT` | `/api/presence/:name` | Set name → pubkey (`{"pubkey":".."}`) |
| `DELETE` | `/api/presence/:name` | Remove mapping |

## Files

```
openwire_server/cloudflare_workers/
├── wrangler.toml       # Workers config + DO bindings
├── package.json
├── src/
│   ├── worker.mjs      # HTTP entry + routes
│   ├── registry.mjs    # DO: relay heartbeat registry
│   ├── presence.mjs    # DO: name→pubkey kv store
│   └── signaling.mjs   # DO: WebSocket signaling room

openwire_core/src/actor/signaling/
└── mod.rs              # SignalingActor (Rust WS client)
```

## Deployment checklist

1. `npx wrangler deploy` — uploads the worker
2. Enable Durable Objects in dashboard
3. (optional) Set `signaling_server` and `signaling_room` in application config
4. (optional) Deploy a relay node on a VPS, register via `POST /api/relays`
5. (optional) Populate bootstrap nodes in `api/bootstrap` by editing `DEFAULT_BOOTSTRAP`