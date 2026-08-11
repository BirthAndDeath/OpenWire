<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { open, save } from "@tauri-apps/plugin-dialog";
  import { onMount } from "svelte";
  import * as THREE from "three";

  interface PeerInfo {
    peer_id: string;
    connected: boolean;
    is_relay: boolean;
    is_bootstrap: boolean;
    is_self: boolean;
  }

  interface NetworkStatus {
    error_code: string;
    error_message: string | null;
    online: boolean;
    is_paid_network: boolean;
    paid_network_mode: string;
    relay_enabled: boolean;
    relay_role: string;
    nat_status: string;
    upnp_status: string;
    ipv4: string[];
    ipv6: string[];
    public_ip: string | null;
    known_peers: PeerInfo[];
    relay_connected: boolean;
    bootstrap_ready: boolean;
    connected_relay_peer: string | null;
    external_addresses: string[];
    local_peer_id: string;
    connected_peer_count: number;
  }

  let status = $state<NetworkStatus | null>(null);
  let loading = $state(true);
  let parseError = $state<string | null>(null);
  let canvasEl = $state<HTMLCanvasElement | null>(null);
  let sceneReady = $state(false);

  let scene: THREE.Scene | null = null;
  let camera: THREE.PerspectiveCamera | null = null;
  let renderer: THREE.WebGLRenderer | null = null;
  let animFrameId: number | null = null;
  let sphereGroup: THREE.Group | null = null;

  async function fetchStatus() {
    loading = true;
    parseError = null;
    try {
      const json = await invoke<string>("get_network_status");
      const parsed = validateNetworkStatus(JSON.parse(json));
      status = parsed;
      if (parsed.error_code !== "OK") {
        parseError = `[${parsed.error_code}] ${parsed.error_message ?? "Unknown error"}`;
      }
    } catch (e) {
      parseError = `[invoke_failed] ${e}`;
      status = null;
    } finally {
      loading = false;
    }
  }

  // B2: 运行时 schema 校验，防止后端结构变化时静默出错
  function validateNetworkStatus(data: unknown): NetworkStatus {
    if (!data || typeof data !== "object") throw new Error("status is not an object");
    const s = data as Record<string, unknown>;
    const needStr = (v: unknown, name: string) => {
      if (typeof v !== "string") throw new Error(`field '${name}' must be a string`);
      return v;
    };
    const needBool = (v: unknown, name: string) => {
      if (typeof v !== "boolean") throw new Error(`field '${name}' must be a boolean`);
      return v;
    };
    const needStrArr = (v: unknown, name: string) => {
      if (!Array.isArray(v) || v.some((x) => typeof x !== "string")) {
        throw new Error(`field '${name}' must be an array of strings`);
      }
      return v;
    };
    const needPeerArr = (v: unknown, name: string) => {
      if (!Array.isArray(v)) throw new Error(`field '${name}' must be an array`);
      return v.map((p) => {
        const pp = p as Record<string, unknown>;
        return {
          peer_id: needStr(pp.peer_id, "peer_id"),
          connected: needBool(pp.connected, "connected"),
          is_relay: needBool(pp.is_relay, "is_relay"),
          is_bootstrap: needBool(pp.is_bootstrap, "is_bootstrap"),
          is_self: needBool(pp.is_self, "is_self"),
        };
      });
    };
    return {
      error_code: needStr(s.error_code, "error_code"),
      error_message: s.error_message === null || typeof s.error_message === "string"
        ? (s.error_message as string | null)
        : null,
      online: needBool(s.online, "online"),
      is_paid_network: needBool(s.is_paid_network, "is_paid_network"),
      paid_network_mode: needStr(s.paid_network_mode, "paid_network_mode"),
      relay_enabled: needBool(s.relay_enabled, "relay_enabled"),
      relay_role: needStr(s.relay_role, "relay_role"),
      nat_status: needStr(s.nat_status, "nat_status"),
      upnp_status: needStr(s.upnp_status, "upnp_status"),
      ipv4: needStrArr(s.ipv4, "ipv4"),
      ipv6: needStrArr(s.ipv6, "ipv6"),
      public_ip: s.public_ip === null || typeof s.public_ip === "string"
        ? (s.public_ip as string | null)
        : null,
      known_peers: needPeerArr(s.known_peers, "known_peers"),
      relay_connected: needBool(s.relay_connected, "relay_connected"),
      bootstrap_ready: needBool(s.bootstrap_ready, "bootstrap_ready"),
      connected_relay_peer: s.connected_relay_peer === null || typeof s.connected_relay_peer === "string"
        ? (s.connected_relay_peer as string | null)
        : null,
      external_addresses: needStrArr(s.external_addresses, "external_addresses"),
      local_peer_id: needStr(s.local_peer_id, "local_peer_id"),
      connected_peer_count: typeof s.connected_peer_count === "number"
        ? (s.connected_peer_count as number)
        : 0,
    };
  }

  // B1/B3: 递归释放 Three.js 节点，覆盖多材质与所有可释放类型
  function disposeNode(obj: THREE.Object3D) {
    if (obj instanceof THREE.Mesh || obj instanceof THREE.LineSegments || obj instanceof THREE.Points) {
      obj.geometry?.dispose();
      const materials = Array.isArray(obj.material) ? obj.material : [obj.material];
      materials.forEach((m) => m?.dispose());
    }
    for (const child of [...obj.children]) {
      disposeNode(child);
    }
  }

  function getColor(peer: PeerInfo): number {
    if (peer.is_self) return 0x3b82f6;
    if (peer.is_relay) return 0xf59e0b;
    if (peer.is_bootstrap) return 0x10b981;
    if (peer.connected) return 0x22c55e;
    return 0x6b7280;
  }

  function getRadius(peer: PeerInfo): number {
    if (peer.is_self) return 0.25;
    if (peer.is_relay) return 0.18;
    return 0.12;
  }

  function updateTopology() {
    if (!scene || !status || !sphereGroup) return;

    while (sphereGroup.children.length > 0) {
      const child = sphereGroup.children[0];
      sphereGroup.remove(child);
      disposeNode(child);
    }

    const peers = status.known_peers;
    const others = peers.filter((p) => !p.is_self);

    if (others.length === 0) {
      const sphere = new THREE.Mesh(
        new THREE.SphereGeometry(0.25, 16, 16),
        new THREE.MeshPhongMaterial({ color: 0x3b82f6, emissive: 0x1d4ed8, emissiveIntensity: 0.3 })
      );
      sphere.position.set(0, 0, 0);
      sphereGroup.add(sphere);
      return;
    }

    const phi = Math.PI * (3 - Math.sqrt(5));
    const positions: THREE.Vector3[] = [];

    for (let i = 0; i < others.length; i++) {
      const y = 1 - (i / (others.length - 1)) * 2;
      const radius = Math.sqrt(1 - y * y);
      const theta = phi * i;
      const x = Math.cos(theta) * radius;
      const z = Math.sin(theta) * radius;
      positions.push(new THREE.Vector3(x, y, z));
    }

    const selfPos = new THREE.Vector3(0, 0, 0);

    for (let i = 0; i < others.length; i++) {
      const peer = others[i];
      const pos = positions[i];
      const color = getColor(peer);
      const r = getRadius(peer);

      const sphere = new THREE.Mesh(
        new THREE.SphereGeometry(r, 12, 12),
        new THREE.MeshPhongMaterial({ color, emissive: color, emissiveIntensity: 0.2 })
      );
      sphere.position.copy(pos);
      sphere.userData = { peerId: peer.peer_id };
      sphereGroup.add(sphere);

      const lineMat = new THREE.LineBasicMaterial({
        color: peer.connected ? 0x22c55e : 0x4b5563,
        transparent: true,
        opacity: peer.connected ? 0.6 : 0.2,
      });
      const lineGeo = new THREE.BufferGeometry().setFromPoints([selfPos, pos]);
      const line = new THREE.Line(lineGeo, lineMat);
      sphereGroup.add(line);
    }

    const selfSphere = new THREE.Mesh(
      new THREE.SphereGeometry(0.25, 16, 16),
      new THREE.MeshPhongMaterial({ color: 0x3b82f6, emissive: 0x1d4ed8, emissiveIntensity: 0.3 })
    );
    selfSphere.position.copy(selfPos);
    sphereGroup.add(selfSphere);
  }

  function initScene() {
    if (!canvasEl) return;
    const w = canvasEl.clientWidth || 400;
    const h = canvasEl.clientHeight || 300;

    scene = new THREE.Scene();
    scene.background = new THREE.Color(0x111111);

    camera = new THREE.PerspectiveCamera(60, w / h, 0.1, 100);
    camera.position.set(0, 0, 4);
    camera.lookAt(0, 0, 0);

    renderer = new THREE.WebGLRenderer({
      canvas: canvasEl,
      antialias: true,
      alpha: true,
    });
    renderer.setSize(w, h);
    renderer.setPixelRatio(Math.min(window.devicePixelRatio, 2));

    const ambient = new THREE.AmbientLight(0x404060, 0.5);
    scene.add(ambient);

    const dirLight = new THREE.DirectionalLight(0xffffff, 1);
    dirLight.position.set(1, 1, 1);
    scene.add(dirLight);

    const backLight = new THREE.DirectionalLight(0x4488ff, 0.3);
    backLight.position.set(-1, -1, -1);
    scene.add(backLight);

    sphereGroup = new THREE.Group();
    scene.add(sphereGroup);

    sceneReady = true;
    animate();
  }

  function animate() {
    if (!renderer || !scene || !camera) return;
    animFrameId = requestAnimationFrame(animate);
    if (sphereGroup) {
      sphereGroup.rotation.y += 0.003;
      sphereGroup.rotation.x += 0.001;
    }
    renderer.render(scene, camera);
  }

  function handleResize() {
    if (!canvasEl || !renderer || !camera) return;
    const w = canvasEl.clientWidth || 400;
    const h = canvasEl.clientHeight || 300;
    camera.aspect = w / h;
    camera.updateProjectionMatrix();
    renderer.setSize(w, h);
  }

  onMount(() => {
    fetchStatus();
    // 初始检测 + 事件驱动计费网络检测，替换 per-fetch 轮询
    const conn = (navigator as any).connection;
    if (conn && typeof conn.addEventListener === "function") {
      autoDetectPaidNetwork();
      const handler = () => autoDetectPaidNetwork();
      conn.addEventListener("change", handler);
      return () => conn.removeEventListener("change", handler);
    }
  });

  $effect(() => {
    if (canvasEl) {
      initScene();
      function onWheel(e: WheelEvent) {
        e.preventDefault();
        if (!camera) return;
        const delta = e.deltaY > 0 ? 1.1 : 0.9;
        camera.position.multiplyScalar(delta);
        const dist = camera.position.length();
        if (dist < 1.5) camera.position.setLength(1.5);
        if (dist > 20) camera.position.setLength(20);
      }
      canvasEl.addEventListener("wheel", onWheel, { passive: false });
      return () => {
        if (animFrameId !== null) cancelAnimationFrame(animFrameId);
        if (renderer) {
          renderer.dispose();
          renderer = null;
        }
        canvasEl?.removeEventListener("wheel", onWheel);
        scene = null;
        camera = null;
        sphereGroup = null;
        sceneReady = false;
      };
    }
  });

  let topologyPeers = $derived(status?.known_peers ?? []);
  $effect(() => {
    if (sceneReady) {
      const _ = topologyPeers.length;
      updateTopology();
    }
  });

  $effect(() => {
    if (canvasEl) {
      window.addEventListener("resize", handleResize);
      return () => window.removeEventListener("resize", handleResize);
    }
  });

  let currentStatus = $derived(status);
  let onlineLabel = $derived(currentStatus ? (currentStatus.online ? "Online" : "Offline") : "Unknown");
  let onlineClass = $derived(currentStatus
    ? (currentStatus.online ? "status-online" : "status-offline")
    : "status-unknown");
  let relayServiceLabel = $derived(currentStatus
    ? (currentStatus.relay_enabled ? "Active" : "Inactive")
    : "Unknown");
  let upnpLabel = $derived(currentStatus
    ? (currentStatus.upnp_status === "Unknown" ? "Unknown" : currentStatus.upnp_status)
    : "Unknown");
  let natLabel = $derived(currentStatus?.nat_status ?? "Unknown");
  let relayLabel = $derived(currentStatus
    ? (currentStatus.relay_connected ? `Connected${currentStatus.connected_relay_peer ? ` (${currentStatus.connected_relay_peer.slice(0, 16)}...)` : ""}` : "Not connected")
    : "Unknown");
  let bootstrapLabel = $derived(currentStatus
    ? (currentStatus.bootstrap_ready ? "Ready" : "Pending")
    : "Unknown");

  let hasError = $derived(parseError !== null);

  let operationMessage = $state<string | null>(null);
  let operationTimer: ReturnType<typeof setTimeout> | null = null;
  let infoOpen = $state(false);

  function showOperation(msg: string) {
    operationMessage = msg;
    if (operationTimer) clearTimeout(operationTimer);
    operationTimer = setTimeout(() => { operationMessage = null; }, 4000);
  }

  // 计费网络检测模式状态（循环：free → paid → disabled → free）
  const PAID_MODES = ["free", "paid", "disabled"] as const;

  async function setPaidMode(mode: string) {
    try {
      await invoke("set_paid_network", { mode });
      await fetchStatus();
      showOperation(mode === "free" ? "Non-metered network — relay allowed"
        : mode === "paid" ? "Paid network — relay disabled"
        : "Network detection disabled — relay off");
    } catch (e) {
      showOperation(`Failed to update network mode: ${e}`);
    }
  }

  // 中继角色循环：server → client → off → server
  const RELAY_ROLES = ["server", "client", "off"] as const;

  async function setRelayRole(role: string) {
    try {
      await invoke("set_relay_role", { role });
      await fetchStatus();
      showOperation(role === "server" ? "Relay role: server — providing relay service"
        : role === "client" ? "Relay role: client — using relays"
        : "Relay role: off — relay hidden from DHT");
    } catch (e) {
      showOperation(`Failed to set relay role: ${e}`);
    }
  }

  // 自动检测计费网络（Navigator NetworkInformation API），非禁用时生效
  async function autoDetectPaidNetwork() {
    if (!status || status.paid_network_mode === "disabled") return;
    try {
      const conn = (navigator as any).connection;
      if (conn && typeof conn.metered === "boolean") {
        const mode = conn.metered ? "paid" : "free";
        if (mode !== status.paid_network_mode) {
          await setPaidMode(mode);
        }
      }
    } catch {
      // Navigator API 不可用，保持当前手动状态
    }
  }

  async function handleExport() {
    try {
      const savePath = await save({
        filters: [{ name: "Routing Table", extensions: ["json"] }],
        defaultPath: `routing-table-export-${Date.now()}.json`,
      });
      if (!savePath) return;
      await invoke("export_routing_table", { savePath });
      showOperation("Routing table exported successfully");
    } catch (e) {
      showOperation(`Export failed: ${e}`);
    }
  }

  async function handleImport() {
    try {
      const selected = await open({
        multiple: false,
        filters: [{ name: "Routing Table", extensions: ["json"] }],
      });
      if (!selected) return;
      const content = await invoke<string>("read_text_file", { path: selected });
      const result = await invoke<string>("import_routing_table", { data: content });
      const parsed = JSON.parse(result);
      const imported = parsed.imported ?? 0;
      const err = parsed.error;
      if (err) {
        showOperation(`Imported ${imported} addresses, errors: ${err}`);
      } else {
        showOperation(`Imported ${imported} addresses into routing table`);
      }
    } catch (e) {
      showOperation(`Import failed: ${e}`);
    }
  }
</script>

<div class="network-monitor">
  <div class="monitor-header">
    <h3>Network Status</h3>
    <div class="header-actions">
      <button class="action-btn info-btn" onclick={() => infoOpen = !infoOpen} aria-label="Info">
        ⓘ
      </button>
      <button class="action-btn export-btn" onclick={handleExport} disabled={loading} aria-label="Export routing table">
        ⬆ Export
      </button>
      <button class="action-btn import-btn" onclick={handleImport} disabled={loading} aria-label="Import routing table">
        ⬇ Import
      </button>
      <button class="refresh-btn" onclick={fetchStatus} disabled={loading} aria-label="Refresh network status">
        <span class="refresh-icon" class:spinning={loading}>↻</span>
        <span>Refresh</span>
      </button>
    </div>
  </div>

  {#if infoOpen}
    <div class="info-panel">
      <strong>Network Detection & Relay</strong>
      <p>This setting controls whether to enable relaying on this device. Relaying helps peers behind NAT connect, but uses data bandwidth.</p>
      <ul>
        <li><strong>Free</strong> — relay allowed (if public). API auto-detection may switch to Paid.</li>
        <li><strong>Paid</strong> — relay disabled. API auto-detection may switch to Free.</li>
        <li><strong>Disabled</strong> — relay permanently off. No auto-detection.</li>
      </ul>
      <p>Click the Network badge to cycle between modes. Default: Paid (conservative, no charges).</p>
    </div>
  {/if}

  {#if hasError}
    <div class="error-banner">
      <span class="error-code-badge">{parseError}</span>
    </div>
  {/if}

  {#if status === null && !hasError && !loading}
    <div class="error-banner">
      <span class="error-code-badge">[no_data] No network status data available</span>
    </div>
  {/if}

  {#if operationMessage}
    <div class="operation-banner">
      {operationMessage}
    </div>
  {/if}

  {#if status !== null}
  <!-- Row 1: Status Bar -->
  <div class="status-bar">
    <div class="status-item">
      <span class="status-label">Connection</span>
      <span class="status-badge {onlineClass}">{onlineLabel}</span>
    </div>
    <div class="status-item">
      <span class="status-label">Network</span>
      <button class="status-badge status-toggle" onclick={() => {
        const modes = PAID_MODES;
        const idx = modes.indexOf(status?.paid_network_mode as any || "paid");
        const next = modes[(idx + 1) % modes.length];
        setPaidMode(next);
      }}
        class:badge-success={status?.paid_network_mode === 'free'}
        class:badge-warning={status?.paid_network_mode === 'paid'}
        class:badge-neutral={status?.paid_network_mode === 'disabled'}
        title={status?.paid_network_mode === 'free' ? 'Non-metered, relay allowed'
          : status?.paid_network_mode === 'paid' ? 'Paid/metered, relay disabled'
          : 'Detection disabled, relay off'}>
        {status?.paid_network_mode === 'free' ? 'Free'
          : status?.paid_network_mode === 'paid' ? 'Paid'
          : 'Disabled'} ⚙
      </button>
    </div>
    <div class="status-item">
      <span class="status-label">Relay Service</span>
      <span class="status-badge" class:badge-success={status?.relay_enabled} class:badge-neutral={!status?.relay_enabled}>{relayServiceLabel}</span>
    </div>
    <div class="status-item">
      <span class="status-label">Role</span>
      <button class="status-badge status-toggle" onclick={() => {
        const idx = RELAY_ROLES.indexOf(status?.relay_role as any || "client");
        const next = RELAY_ROLES[(idx + 1) % RELAY_ROLES.length];
        setRelayRole(next);
      }}
        class:badge-success={status?.relay_role === 'server'}
        class:badge-warning={status?.relay_role === 'client'}
        class:badge-neutral={status?.relay_role === 'off'}
        title={status?.relay_role === 'server' ? 'Providing relay service, not using relays'
          : status?.relay_role === 'client' ? 'Using relays, not providing service'
          : 'Relay hidden from DHT (not fully disabled)'}>
        {status?.relay_role === 'server' ? 'Server'
          : status?.relay_role === 'client' ? 'Client'
          : 'Off'} ⚙
      </button>
    </div>
    <div class="status-item">
      <span class="status-label">NAT</span>
      <span class="status-badge"
        class:badge-success={natLabel === 'Public'}
        class:badge-warning={natLabel === 'Private'}
        class:badge-neutral={natLabel === 'Unknown'}>{natLabel}</span>
    </div>
  </div>

  <!-- Row 2: Network Info -->
  <div class="network-info">
    <div class="info-section">
      <h4>IPv4 {#if status?.ipv4?.length}<span class="addr-count">({status.ipv4.length})</span>{/if}</h4>
      <div class="addr-list">
        {#if status?.ipv4?.length}
          {#each status.ipv4 as addr}
            <span class="addr-item">{addr}</span>
          {/each}
        {:else}
          <span class="addr-item addr-empty">None</span>
        {/if}
      </div>
    </div>
    <div class="info-section">
      <h4>IPv6 {#if status?.ipv6?.length}<span class="addr-count">({status.ipv6.length})</span>{/if}</h4>
      <div class="addr-list">
        {#if status?.ipv6?.length}
          {#each status.ipv6 as addr}
            <span class="addr-item">{addr}</span>
          {/each}
        {:else}
          <span class="addr-item addr-empty">None</span>
        {/if}
      </div>
    </div>
    <div class="info-section">
      <h4>Public Address</h4>
      <span class="addr-item" class:addr-empty={!status?.public_ip}>{status?.public_ip ?? "None"}</span>
    </div>
    <div class="info-section">
      <h4>UPnP</h4>
      <span class="status-badge" class:badge-success={status?.upnp_status === 'Enabled'} class:badge-neutral={status?.upnp_status !== 'Enabled'}>{upnpLabel}</span>
    </div>
    <div class="info-section">
      <h4>Bootstrap</h4>
      <span class="status-badge" class:badge-success={status?.bootstrap_ready} class:badge-neutral={!status?.bootstrap_ready}>{bootstrapLabel}</span>
    </div>
    <div class="info-section">
      <h4>Relay</h4>
      <span class="status-badge" class:badge-success={status?.relay_connected} class:badge-neutral={!status?.relay_connected}>{relayLabel}</span>
    </div>
    <div class="info-section">
      <h4>Peers</h4>
      <span class="addr-item">{status?.connected_peer_count ?? 0} connected</span>
    </div>
  </div>
{/if}

  <!-- Row 3: 3D Topology -->
  <div class="topology-section">
    <h4>Network Topology</h4>
    <div class="topology-canvas-wrapper">
      {#if status && status.known_peers.length > 0}
        <canvas bind:this={canvasEl} class="topology-canvas"></canvas>
      {:else}
        <div class="topology-empty-state">
          <span class="empty-icon">◉</span>
          {#if status?.error_code === "not_ready"}
            <span>Core initializing — no peers discovered yet</span>
          {:else if status?.error_code === "core_not_initialized"}
            <span>Core is not initialized</span>
          {:else}
            <span>No peers in topology</span>
          {/if}
        </div>
      {/if}
      {#if loading}
        <div class="topology-loading-overlay">
          <span class="loading-spinner">⟳</span>
          <span>Loading topology...</span>
        </div>
      {/if}
      {#if status && status.known_peers.length > 0}
        <div class="topology-legend">
          <span class="legend-item"><span class="legend-dot" style="background:#3b82f6"></span>Self</span>
          <span class="legend-item"><span class="legend-dot" style="background:#22c55e"></span>Connected</span>
          <span class="legend-item"><span class="legend-dot" style="background:#f59e0b"></span>Relay</span>
          <span class="legend-item"><span class="legend-dot" style="background:#10b981"></span>Bootstrap</span>
        </div>
      {/if}
    </div>
  </div>
</div>

<style>
  .network-monitor {
    background: var(--bg-tertiary, #1a1a1a);
    border: 1px solid var(--border-color, #2a2a2a);
    border-radius: 8px;
    padding: 16px;
    margin-bottom: 24px;
  }

  .monitor-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 16px;
  }

  .monitor-header h3 {
    margin: 0;
    font-size: 16px;
    font-weight: 600;
    color: var(--text-primary, #fafafa);
  }

  .header-actions {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .action-btn {
    display: flex;
    align-items: center;
    background: transparent;
    border: 1px solid var(--border-color, #2a2a2a);
    border-radius: 6px;
    padding: 6px 10px;
    color: var(--text-primary, #fafafa);
    cursor: pointer;
    font-size: 12px;
    transition: all 0.2s;
  }

  .action-btn:hover:not(:disabled) {
    background: rgba(59, 130, 246, 0.1);
    border-color: #3b82f6;
    color: #3b82f6;
  }

  .action-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .export-btn:hover:not(:disabled) {
    border-color: #10b981;
    color: #10b981;
    background: rgba(16, 185, 129, 0.1);
  }

  .import-btn:hover:not(:disabled) {
    border-color: #f59e0b;
    color: #f59e0b;
    background: rgba(245, 158, 11, 0.1);
  }

  .refresh-btn {
    display: flex;
    align-items: center;
    gap: 6px;
    background: transparent;
    border: 1px solid var(--border-color, #2a2a2a);
    border-radius: 6px;
    padding: 6px 12px;
    color: var(--text-primary, #fafafa);
    cursor: pointer;
    font-size: 12px;
    transition: all 0.2s;
  }

  .refresh-btn:hover:not(:disabled) {
    border-color: #3b82f6;
    color: #3b82f6;
    background: rgba(59, 130, 246, 0.1);
  }

  .refresh-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .refresh-icon {
    display: inline-block;
    font-size: 16px;
    transition: transform 0.2s;
  }

  .refresh-icon.spinning {
    animation: spin 1s linear infinite;
  }

  @keyframes spin {
    from { transform: rotate(0deg); }
    to { transform: rotate(360deg); }
  }

  .error-banner {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 14px;
    margin-bottom: 12px;
    background: rgba(239, 68, 68, 0.1);
    border: 1px solid rgba(239, 68, 68, 0.2);
    border-radius: 6px;
    color: #ef4444;
    font-size: 13px;
    font-family: monospace;
  }

  .error-code-badge {
    display: inline-block;
    padding: 2px 6px;
    background: rgba(239, 68, 68, 0.2);
    border-radius: 3px;
    font-size: 12px;
    font-weight: 600;
    font-family: monospace;
  }

  .operation-banner {
    padding: 8px 14px;
    margin-bottom: 12px;
    background: rgba(16, 185, 129, 0.1);
    border: 1px solid rgba(16, 185, 129, 0.2);
    border-radius: 6px;
    color: #10b981;
    font-size: 13px;
    font-family: monospace;
  }

  .info-btn {
    font-size: 16px;
    padding: 4px 10px;
  }

  .info-btn:hover:not(:disabled) {
    border-color: #6b7280;
    color: #6b7280;
    background: rgba(107, 114, 128, 0.1);
  }

  .info-panel {
    padding: 12px 14px;
    margin-bottom: 12px;
    background: rgba(59, 130, 246, 0.05);
    border: 1px solid rgba(59, 130, 246, 0.15);
    border-radius: 6px;
    color: var(--text-primary, #fafafa);
    font-size: 12px;
    line-height: 1.6;
  }

  .info-panel strong {
    color: #3b82f6;
    font-size: 13px;
  }

  .info-panel ul {
    margin: 6px 0;
    padding-left: 18px;
  }

  .info-panel li {
    margin-bottom: 4px;
  }

  .status-bar {
    display: flex;
    flex-wrap: wrap;
    gap: 12px;
    margin-bottom: 16px;
    padding: 12px;
    background: var(--bg-secondary, #0a0a0a);
    border: 1px solid var(--border-color, #2a2a2a);
    border-radius: 6px;
  }

  .status-item {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .status-label {
    font-size: 12px;
    color: var(--text-secondary, #737373);
    white-space: nowrap;
  }

  .status-badge {
    display: inline-flex;
    align-items: center;
    padding: 2px 8px;
    border-radius: 4px;
    font-size: 11px;
    font-weight: 600;
    white-space: nowrap;
  }

  .status-online {
    background: rgba(34, 197, 94, 0.15);
    color: #22c55e;
    border: 1px solid rgba(34, 197, 94, 0.3);
  }

  .status-offline {
    background: rgba(239, 68, 68, 0.15);
    color: #ef4444;
    border: 1px solid rgba(239, 68, 68, 0.3);
  }

  .status-unknown {
    background: rgba(107, 114, 128, 0.15);
    color: #6b7280;
    border: 1px solid rgba(107, 114, 128, 0.3);
  }

  .badge-success {
    background: rgba(34, 197, 94, 0.15);
    color: #22c55e;
    border: 1px solid rgba(34, 197, 94, 0.3);
  }

  .badge-warning {
    background: rgba(245, 158, 11, 0.15);
    color: #f59e0b;
    border: 1px solid rgba(245, 158, 11, 0.3);
  }

  .badge-neutral {
    background: rgba(107, 114, 128, 0.15);
    color: #9ca3af;
    border: 1px solid rgba(107, 114, 128, 0.3);
  }

  .status-toggle {
    cursor: pointer;
    transition: all 0.2s;
  }

  .status-toggle:hover {
    filter: brightness(1.3);
    transform: scale(1.05);
  }

  .network-info {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(160px, 1fr));
    gap: 10px;
    margin-bottom: 16px;
    padding: 12px;
    background: var(--bg-secondary, #0a0a0a);
    border: 1px solid var(--border-color, #2a2a2a);
    border-radius: 6px;
  }

  .info-section {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .info-section h4 {
    margin: 0;
    font-size: 11px;
    font-weight: 600;
    color: var(--text-secondary, #737373);
    text-transform: uppercase;
    letter-spacing: 0.5px;
  }

  .addr-count {
    color: var(--text-secondary, #555);
    font-weight: 400;
  }

  .addr-list {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .addr-item {
    font-family: monospace;
    font-size: 11px;
    color: var(--text-primary, #fafafa);
    word-break: break-all;
  }

  .addr-empty {
    color: var(--text-secondary, #555);
    font-style: italic;
  }

  .topology-section {
    margin-top: 4px;
  }

  .topology-section h4 {
    margin: 0 0 8px 0;
    font-size: 12px;
    font-weight: 600;
    color: var(--text-secondary, #737373);
    text-transform: uppercase;
    letter-spacing: 0.5px;
  }

  .topology-canvas-wrapper {
    position: relative;
    width: 100%;
    height: 260px;
    border: 1px solid var(--border-color, #2a2a2a);
    border-radius: 6px;
    overflow: hidden;
  }

  .topology-canvas {
    display: block;
    width: 100%;
    height: 100%;
  }

  .topology-empty-state {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 8px;
    color: var(--text-secondary, #555);
    font-size: 13px;
  }

  .empty-icon {
    font-size: 32px;
    opacity: 0.3;
  }

  .topology-loading-overlay {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 8px;
    background: rgba(17, 17, 17, 0.8);
    color: var(--text-secondary, #737373);
    font-size: 13px;
  }

  .loading-spinner {
    font-size: 24px;
    animation: spin 1s linear infinite;
  }

  .topology-legend {
    position: absolute;
    bottom: 8px;
    right: 8px;
    display: flex;
    gap: 10px;
    padding: 6px 10px;
    background: rgba(0, 0, 0, 0.7);
    border-radius: 4px;
    font-size: 10px;
    color: var(--text-secondary, #9ca3af);
  }

  .legend-item {
    display: flex;
    align-items: center;
    gap: 4px;
  }

  .legend-dot {
    display: inline-block;
    width: 8px;
    height: 8px;
    border-radius: 50%;
  }
</style>