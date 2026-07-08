const RELAY_TTL_MS = 120_000;

export class RelayRegistry {
  constructor(state) {
    this.state = state;
    this.relays = new Map();
  }

  async fetch(request) {
    let url = new URL(request.url);

    switch (url.pathname) {
      case "/list": {
        let now = Date.now();
        let active = [];
        for (let [id, r] of this.relays) {
          if (now - r.lastSeen > RELAY_TTL_MS) {
            this.relays.delete(id);
            continue;
          }
          active.push([r.id, r.addr]);
        }
        return new Response(JSON.stringify(active));
      }

      case "/register": {
        let body = await request.json();
        if (!body.id || !body.addr) {
          return new Response(JSON.stringify({ error: "missing id or addr" }), { status: 400 });
        }
        this.relays.set(body.id, { id: body.id, addr: body.addr, lastSeen: Date.now() });
        return new Response(JSON.stringify({ id: body.id, addr: body.addr, status: "registered" }));
      }

      case "/ping": {
        let body = await request.json();
        let r = this.relays.get(body.id);
        if (!r) {
          return new Response(JSON.stringify({ error: "unknown relay" }), { status: 404 });
        }
        r.lastSeen = Date.now();
        return new Response(JSON.stringify({ id: r.id, status: "refreshed" }));
      }

      default:
        return new Response("not found", { status: 404 });
    }
  }
}