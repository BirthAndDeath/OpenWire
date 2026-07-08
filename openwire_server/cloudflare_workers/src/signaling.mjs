// =============================================================================
// SignalingRoom — Durable Object
//
// Lightweight WebSocket signaling exchange for NAT traversal.
// Clients connect via WebSocket, send their PeerId + Multiaddrs,
// the room relays that info to all other connected clients.
//
// Protocol (JSON over WS):
//   → {"type":"register","peer_id":"...","addrs":["...","..."]}
//   ← {"type":"peer","peer_id":"...","addrs":["...","..."]}
//   → {"type":"signal","target":"...","data":"..."}   // direct signaling
//   ← {"type":"signal","from":"...","data":"..."}
//
// No data relay — only address exchange. Actual libp2p connection
// happens directly between peers after they dial each other.
// =============================================================================

export class SignalingRoom {
  constructor(state) {
    this.state = state;
    // Map<webSocket, { peerId, addrs }>
    this.sessions = new Map();

    // Restore sessions after hibernation wakeup
    this.state.getWebSockets().forEach(ws => {
      let meta = ws.deserializeAttachment();
      if (meta) {
        this.sessions.set(ws, { peerId: meta.peerId, addrs: meta.addrs || [] });
      }
    });
  }

  async fetch(request) {
    if (request.headers.get("Upgrade") !== "websocket") {
      return new Response("expected websocket", { status: 400 });
    }

    let pair = new WebSocketPair();
    this.state.acceptWebSocket(pair[1]);

    this.sessions.set(pair[1], { peerId: null, addrs: [] });

    return new Response(null, { status: 101, webSocket: pair[0] });
  }

  async webSocketMessage(ws, msg) {
    let session = this.sessions.get(ws);
    if (!session) return;

    try {
      let data = JSON.parse(msg);

      switch (data.type) {

        case "register": {
          session.peerId = data.peer_id;
          session.addrs = data.addrs || [];
          ws.serializeAttachment({ peerId: session.peerId, addrs: session.addrs });

          // Tell everyone else about this new peer
          this.broadcast({
            type: "peer",
            peer_id: session.peerId,
            addrs: session.addrs,
          }, ws);

          // Tell the new peer about everyone already in the room
          for (let [other, info] of this.sessions) {
            if (other !== ws && info.peerId) {
              ws.send(JSON.stringify({
                type: "peer",
                peer_id: info.peerId,
                addrs: info.addrs,
              }));
            }
          }

          console.log(`registered peer=${session.peerId} addrs=${session.addrs.length}`);
          break;
        }

        case "signal": {
          // Forward a direct signal to a specific peer
          for (let [other, info] of this.sessions) {
            if (other !== ws && info.peerId === data.target) {
              other.send(JSON.stringify({
                type: "signal",
                from: session.peerId,
                data: data.data,
              }));
              break;
            }
          }
          break;
        }

        default:
          ws.send(JSON.stringify({ type: "error", error: "unknown message type" }));
      }
    } catch (err) {
      ws.send(JSON.stringify({ type: "error", error: err.message }));
    }
  }

  async webSocketClose(ws) {
    let session = this.sessions.get(ws);
    this.sessions.delete(ws);
    if (session && session.peerId) {
      this.broadcast({ type: "peer_left", peer_id: session.peerId });
    }
  }

  async webSocketError(ws) {
    this.webSocketClose(ws);
  }

  broadcast(message, exclude) {
    let str = typeof message === "string" ? message : JSON.stringify(message);
    for (let [other] of this.sessions) {
      if (other !== exclude) {
        try {
          other.send(str);
        } catch (_) {
          this.sessions.delete(other);
        }
      }
    }
  }
}