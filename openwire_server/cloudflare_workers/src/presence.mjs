export class Presence {
  constructor(state, env) {
    this.state = state;
    this.storage = state.storage;
  }

  async fetch(request) {
    let url = new URL(request.url);

    switch (url.pathname) {
      case "/get": {
        let pubkey = await this.storage.get("pubkey");
        return new Response(JSON.stringify({ pubkey: pubkey || null }));
      }
      case "/set": {
        let body = await request.json();
        await this.storage.put("pubkey", body.pubkey);
        return new Response(JSON.stringify({ status: "saved" }));
      }
      case "/delete": {
        await this.storage.delete("pubkey");
        return new Response(JSON.stringify({ status: "deleted" }));
      }
      default:
        return new Response("not found", { status: 404 });
    }
  }
}