<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { open, save } from "@tauri-apps/plugin-dialog";
  import { onMount } from "svelte";
  import { _ } from "svelte-i18n";
  import { tt } from "../i18n";
  import Spinner from "../components/Spinner.svelte";

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

  let dialPeerId = $state("");
  let dialAddr = $state("");
  let dialing = $state(false);
  let dialMessage = $state("");
  let dialMessageOk = $state(false);

  async function copyPeerId() {
    if (!status?.local_peer_id) return;
    try {
      await navigator.clipboard.writeText(status.local_peer_id);
    } catch {
      // fallback for non-secure contexts
    }
  }

  async function handleDial() {
    const pid = dialPeerId.trim();
    const addr = dialAddr.trim();
    if (!pid || !addr) return;
    dialing = true;
    dialMessage = "";
    try {
      await invoke("dial_peer", { peerId: pid, addr });
      dialMessage = tt("netmon_dial_sent");
      dialMessageOk = true;
      dialPeerId = "";
      dialAddr = "";
    } catch (e) {
      dialMessage = `${tt("netmon_dial_failed")}: ${e}`;
      dialMessageOk = false;
    } finally {
      dialing = false;
    }
  }

  let animFrameId: number | null = null;
  let rotation = 0;
  let bgColor = "#111111";

  async function fetchStatus() {
    loading = true;
    parseError = null;
    try {
      const json = await invoke<string>("get_network_status");
      const parsed = validateNetworkStatus(JSON.parse(json));
      status = parsed;
      if (parsed.error_code !== "OK") {
        parseError = `[${parsed.error_code}] ${parsed.error_message ?? tt("unknown_error")}`;
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

  function getColor(peer: PeerInfo): string {
    if (peer.is_self) return "#3b82f6";
    if (!peer.connected) return "#6b7280";
    if (peer.is_bootstrap) return "#ef4444";
    if (peer.is_relay) return "#f59e0b";
    return "#22c55e";
  }

  function getRadius(peer: PeerInfo): number {
    if (peer.is_self) return 0.25;
    if (peer.is_relay) return 0.18;
    return 0.12;
  }

  function drawScene() {
    if (!canvasEl || !status) return;
    const ctx = canvasEl.getContext("2d");
    if (!ctx) return;
    const w = canvasEl.clientWidth || 400;
    const h = canvasEl.clientHeight || 300;
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    const bw = Math.round(w * dpr);
    const bh = Math.round(h * dpr);
    if (canvasEl.width !== bw || canvasEl.height !== bh) {
      canvasEl.width = bw;
      canvasEl.height = bh;
    }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

    ctx.fillStyle = bgColor;
    ctx.fillRect(0, 0, w, h);

    const peers = status.known_peers;
    const others = peers.filter((p) => !p.is_self);
    if (others.length === 0) {
      const cx = w / 2, cy = h / 2;
      ctx.beginPath();
      ctx.arc(cx, cy, 8, 0, Math.PI * 2);
      ctx.fillStyle = "#3b82f6";
      ctx.fill();
      return;
    }

    const phi = Math.PI * (3 - Math.sqrt(5));
    const cx = w / 2, cy = h / 2;
    const scale = Math.min(w, h) * 0.35;
    const rot = rotation;

    const sx = (x: number, z: number) => cx + (x * Math.cos(rot) + z * Math.sin(rot)) * scale;
    const sy = (y: number, z: number) => cy + (y * Math.cos(rot) - z * Math.sin(rot) * 0.3) * scale;

    const pts: { x: number; y: number; z: number; peer: PeerInfo; r: number }[] = [];

    for (let i = 0; i < others.length; i++) {
      const y = 1 - (i / (others.length - 1)) * 2;
      const radius = Math.sqrt(1 - y * y);
      const theta = phi * i;
      const x = Math.cos(theta) * radius;
      const z = Math.sin(theta) * radius;
      const peer = others[i];
      pts.push({ x, y, z, peer, r: getRadius(peer) });
    }

    const s_cx = cx, s_cy = cy;

    for (const p of pts) {
      const px = sx(p.x, p.z);
      const py = sy(p.y, p.z);
      ctx.beginPath();
      ctx.moveTo(s_cx, s_cy);
      ctx.lineTo(px, py);
      ctx.strokeStyle = p.peer.connected ? "rgba(34,197,94,0.4)" : "rgba(75,85,99,0.15)";
      ctx.lineWidth = 1;
      ctx.stroke();
    }

    for (const p of pts) {
      const px = sx(p.x, p.z);
      const py = sy(p.y, p.z);
      const screenR = Math.max(3, p.r * scale * 0.5);
      ctx.beginPath();
      ctx.arc(px, py, screenR, 0, Math.PI * 2);
      ctx.fillStyle = getColor(p.peer);
      ctx.fill();
    }

    ctx.beginPath();
    ctx.arc(s_cx, s_cy, 8, 0, Math.PI * 2);
    ctx.fillStyle = "#3b82f6";
    ctx.fill();
    ctx.beginPath();
    ctx.arc(s_cx, s_cy, 10, 0, Math.PI * 2);
    ctx.strokeStyle = "rgba(59,130,246,0.5)";
    ctx.lineWidth = 2;
    ctx.stroke();
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
      function loop() {
        rotation += 0.003;
        drawScene();
        animFrameId = requestAnimationFrame(loop);
      }
      animFrameId = requestAnimationFrame(loop);
      return () => {
        if (animFrameId !== null) cancelAnimationFrame(animFrameId);
      };
    }
  });

  let currentStatus = $derived(status);
  let onlineLabel = $derived(currentStatus
    ? (currentStatus.online ? $_("netmon_online") : $_("netmon_offline"))
    : $_("netmon_unknown"));
  let onlineClass = $derived(currentStatus
    ? (currentStatus.online ? "status-online" : "status-offline")
    : "status-unknown");
  let relayServiceLabel = $derived(currentStatus
    ? (currentStatus.relay_enabled ? $_("netmon_active") : $_("netmon_inactive"))
    : $_("netmon_unknown"));
  let upnpLabel = $derived(currentStatus
    ? (currentStatus.upnp_status === "Enabled" ? $_("netmon_enabled")
      : currentStatus.upnp_status === "Disabled" ? $_("netmon_disabled")
      : $_("netmon_unknown"))
    : $_("netmon_unknown"));
  let natLabel = $derived(currentStatus
    ? (currentStatus.nat_status === "Public" ? $_("netmon_public")
      : currentStatus.nat_status === "Private" ? $_("netmon_private")
      : $_("netmon_unknown"))
    : $_("netmon_unknown"));
  // 原始 NAT 状态值（用于 badge 颜色判断，与本地化显示分离）
  let rawNatStatus = $derived(currentStatus?.nat_status ?? "Unknown");
  let relayLabel = $derived(currentStatus
    ? (currentStatus.relay_connected
        ? `${$_("netmon_connected")}${currentStatus.connected_relay_peer ? ` (${currentStatus.connected_relay_peer.slice(0, 16)}...)` : ""}`
        : $_("netmon_not_connected"))
    : $_("netmon_unknown"));
  let bootstrapLabel = $derived(currentStatus
    ? (currentStatus.bootstrap_ready ? $_("netmon_ready") : $_("netmon_pending"))
    : $_("netmon_unknown"));

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
      showOperation(mode === "free" ? tt("netmon_op_mode_free")
        : mode === "paid" ? tt("netmon_op_mode_paid")
        : tt("netmon_op_mode_disabled"));
    } catch (e) {
      showOperation(`${tt("netmon_failed_update")}: ${e}`);
    }
  }

  // 中继角色循环：server → client → off → server
  const RELAY_ROLES = ["server", "client", "off"] as const;

  async function setRelayRole(role: string) {
    try {
      await invoke("set_relay_role", { role });
      await fetchStatus();
      showOperation(role === "server" ? tt("netmon_op_role_server")
        : role === "client" ? tt("netmon_op_role_client")
        : tt("netmon_op_role_off"));
    } catch (e) {
      showOperation(`${tt("netmon_failed_set_role")}: ${e}`);
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
        filters: [{ name: tt("routing_table"), extensions: ["json"] }],
        defaultPath: `routing-table-export-${Date.now()}.json`,
      });
      if (!savePath) return;
      await invoke("export_routing_table", { savePath });
      showOperation(tt("routing_table_exported"));
    } catch (e) {
      showOperation(tt("export_failed", { values: { error: e } }));
    }
  }

  async function handleImport() {
    try {
      const selected = await open({
        multiple: false,
        filters: [{ name: tt("routing_table"), extensions: ["json"] }],
      });
      if (!selected) return;
      const content = await invoke<string>("read_text_file", { path: selected });
      const result = await invoke<string>("import_routing_table", { data: content });
      const parsed = JSON.parse(result);
      const imported = parsed.imported ?? 0;
      const err = parsed.error;
      if (err) {
        showOperation(tt("imported_with_errors", { values: { count: imported, error: err } }));
      } else {
        showOperation(tt("imported_success", { values: { count: imported } }));
      }
    } catch (e) {
      showOperation(tt("import_failed", { values: { error: e } }));
    }
  }
</script>

<div class="network-monitor">
  <div class="monitor-header">
    <h3>{$_("netmon_network_status")}</h3>
    <div class="header-actions">
      <button class="action-btn info-btn" onclick={() => infoOpen = !infoOpen} aria-label={$_("aria_info")}>
        ⓘ
      </button>
      <button class="action-btn export-btn" onclick={handleExport} disabled={loading} aria-label={$_("export_routing_table")}>
        ⬆ {$_("netmon_export")}
      </button>
      <button class="action-btn import-btn" onclick={handleImport} disabled={loading} aria-label={$_("import_routing_table")}>
        ⬇ {$_("netmon_import")}
      </button>
      <button class="refresh-btn" onclick={fetchStatus} disabled={loading} aria-label={$_("refresh_network_status")}>
        {#if loading}
          <Spinner size={16} />
        {:else}
          <span class="refresh-icon">↻</span>
        {/if}
        <span>{$_("netmon_refresh")}</span>
      </button>
    </div>
  </div>

  {#if infoOpen}
    <div class="info-panel">
      <strong>{$_("netmon_info_title")}</strong>
      <p>{$_("netmon_info_intro")}</p>
      <ul>
        <li><strong>{$_("netmon_mode_free")}</strong> — {$_("netmon_info_free")}</li>
        <li><strong>{$_("netmon_mode_paid")}</strong> — {$_("netmon_info_paid")}</li>
        <li><strong>{$_("netmon_mode_disabled")}</strong> — {$_("netmon_info_disabled")}</li>
      </ul>
      <p>{$_("netmon_info_default")}</p>
      <p><em>{$_("netmon_info_note")}</em></p>
    </div>
  {/if}

  {#if hasError}
    <div class="error-banner">
      <span class="error-code-badge">{parseError}</span>
    </div>
  {/if}

  {#if status === null && !hasError && !loading}
    <div class="error-banner">
      <span class="error-code-badge">[no_data] {$_("no_network_data")}</span>
    </div>
  {/if}

  {#if loading && status === null}
    <div class="loading-skeleton">
      <div class="skeleton-row">
        {#each [$_( "netmon_connection"), $_("netmon_network"), $_("netmon_relay_service"), $_("netmon_role"), $_("netmon_nat")] as label}
          <div class="skeleton-item">
            <span class="skeleton-label">{label}</span>
            <span class="skeleton-badge pulse"></span>
          </div>
        {/each}
      </div>
      <div class="skeleton-topology">
        <div class="skeleton-title pulse"></div>
        <div class="skeleton-canvas pulse"></div>
      </div>
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
      <span class="status-label">{$_("netmon_connection")}</span>
      <span class="status-badge {onlineClass}">{onlineLabel}</span>
    </div>
    <div class="status-item">
      <span class="status-label">{$_("netmon_network")}</span>
      <button class="status-badge status-toggle" onclick={() => {
        const modes = PAID_MODES;
        const idx = modes.indexOf(status?.paid_network_mode as any || "disabled");
        const next = modes[(idx + 1) % modes.length];
        setPaidMode(next);
      }}
        class:badge-success={status?.paid_network_mode === 'free'}
        class:badge-warning={status?.paid_network_mode === 'paid'}
        class:badge-neutral={status?.paid_network_mode === 'disabled'}
        title={status?.paid_network_mode === 'free' ? $_("netmon_mode_free_title")
          : status?.paid_network_mode === 'paid' ? $_("netmon_mode_paid_title")
          : $_("netmon_mode_disabled_title")}>
        {status?.paid_network_mode === 'free' ? $_("netmon_mode_free")
          : status?.paid_network_mode === 'paid' ? $_("netmon_mode_paid")
          : $_("netmon_mode_disabled")} ⚙
      </button>
    </div>
    <div class="status-item">
      <span class="status-label">{$_("netmon_relay_service")}</span>
      <span class="status-badge" class:badge-success={status?.relay_enabled} class:badge-neutral={!status?.relay_enabled}>{relayServiceLabel}</span>
    </div>
    <div class="status-item">
      <span class="status-label">{$_("netmon_role")}</span>
      <button class="status-badge status-toggle" onclick={() => {
        const idx = RELAY_ROLES.indexOf(status?.relay_role as any || "client");
        const next = RELAY_ROLES[(idx + 1) % RELAY_ROLES.length];
        setRelayRole(next);
      }}
        class:badge-success={status?.relay_role === 'server'}
        class:badge-warning={status?.relay_role === 'client'}
        class:badge-neutral={status?.relay_role === 'off'}
        title={status?.relay_role === 'server' ? $_("netmon_role_server_title")
          : status?.relay_role === 'client' ? $_("netmon_role_client_title")
          : $_("netmon_role_off_title")}>
        {status?.relay_role === 'server' ? $_("netmon_role_server")
          : status?.relay_role === 'client' ? $_("netmon_role_client")
          : $_("netmon_role_off")} ⚙
      </button>
    </div>
    <div class="status-item">
      <span class="status-label">{$_("netmon_nat")}</span>
      <span class="status-badge"
        class:badge-success={rawNatStatus === 'Public'}
        class:badge-warning={rawNatStatus === 'Private'}
        class:badge-neutral={rawNatStatus === 'Unknown'}>{natLabel}</span>
    </div>
  </div>

  <!-- Row 2: Network Info -->
  <div class="network-info">
    <div class="info-section">
      <h4>{$_("netmon_ipv4")} {#if status?.ipv4?.length}<span class="addr-count">({status.ipv4.length})</span>{/if}</h4>
      <div class="addr-list">
        {#if status?.ipv4?.length}
          {#each status.ipv4 as addr}
            <span class="addr-item">{addr}</span>
          {/each}
        {:else}
          <span class="addr-item addr-empty">{$_("netmon_none")}</span>
        {/if}
      </div>
    </div>
    <div class="info-section">
      <h4>{$_("netmon_ipv6")} {#if status?.ipv6?.length}<span class="addr-count">({status.ipv6.length})</span>{/if}</h4>
      <div class="addr-list">
        {#if status?.ipv6?.length}
          {#each status.ipv6 as addr}
            <span class="addr-item">{addr}</span>
          {/each}
        {:else}
          <span class="addr-item addr-empty">{$_("netmon_none")}</span>
        {/if}
      </div>
    </div>
    <div class="info-section">
      <h4>{$_("netmon_public_address")}</h4>
      <span class="addr-item" class:addr-empty={!status?.public_ip}>{status?.public_ip ?? $_("netmon_none")}</span>
    </div>
    <div class="info-section">
      <h4>{$_("netmon_upnp")}</h4>
      <span class="status-badge" class:badge-success={status?.upnp_status === 'Enabled'} class:badge-neutral={status?.upnp_status !== 'Enabled'}>{upnpLabel}</span>
    </div>    <div class="info-section">
      <h4>{$_("netmon_bootstrap")}</h4>
      <span class="status-badge" class:badge-success={status?.bootstrap_ready} class:badge-neutral={!status?.bootstrap_ready}>{bootstrapLabel}</span>
    </div>
    <div class="info-section">
      <h4>{$_("netmon_relay")}</h4>
      <span class="status-badge" class:badge-success={status?.relay_connected} class:badge-neutral={!status?.relay_connected}>{relayLabel}</span>
    </div>
    <div class="info-section">
      <h4>{$_("netmon_peers")}</h4>
      <span class="addr-item">{$_("netmon_connected_count", { values: { n: status?.connected_peer_count ?? 0 } })}</span>
    </div>
    <div class="info-section">
      <h4>{$_("netmon_peerid")}</h4>
      <span class="addr-item peerid-display" onclick={copyPeerId} onkeydown={(e) => e.key === 'Enter' && copyPeerId()} role="button" tabindex="0" title={$_("netmon_click_to_copy")}>
        {status?.local_peer_id ?? $_("netmon_unknown")}
      </span>
    </div>
    <div class="info-section">
      <h4>{$_("netmon_multiaddrs")} {#if status?.external_addresses?.length}<span class="addr-count">({status.external_addresses.length})</span>{/if}</h4>
      <div class="addr-list">
        {#if status?.external_addresses?.length}
          {#each status.external_addresses as maddr}
            <span class="addr-item">{maddr}</span>
          {/each}
        {:else}
          <span class="addr-item addr-empty">{$_("netmon_none")}</span>
        {/if}
      </div>
    </div>
  </div>
{/if}

  <!-- Row 3: 3D Topology -->
  <div class="topology-section">
    <h4>{$_("netmon_topology")}</h4>
    <div class="topology-canvas-wrapper">
      {#if status && status.known_peers.length > 0}
        <canvas bind:this={canvasEl} class="topology-canvas"></canvas>
      {:else}
        <div class="topology-empty-state">
          <span class="empty-icon">◉</span>
          {#if status?.error_code === "not_ready"}
            <span>{$_("netmon_core_initializing")}</span>
          {:else if status?.error_code === "core_not_initialized"}
            <span>{$_("netmon_core_not_initialized")}</span>
          {:else}
            <span>{$_("netmon_no_peers")}</span>
          {/if}
        </div>
      {/if}
      {#if loading && status !== null}
        <div class="topology-loading-overlay">
          <Spinner size={24} />
          <span>{$_("netmon_loading_topology")}</span>
        </div>
      {/if}
      {#if status && status.known_peers.length > 0}
        <div class="topology-legend">
          <span class="legend-item"><span class="legend-dot" style="background:#3b82f6"></span>{$_("netmon_self")}</span>
          <span class="legend-item"><span class="legend-dot" style="background:#6b7280"></span>{$_("netmon_disconnected")}</span>
          <span class="legend-item"><span class="legend-dot" style="background:#22c55e"></span>{$_("netmon_connected")}</span>
          <span class="legend-item"><span class="legend-dot" style="background:#ef4444"></span>{$_("netmon_bootstrap")}</span>
          <span class="legend-item"><span class="legend-dot" style="background:#f59e0b"></span>{$_("netmon_relay")}</span>
        </div>
      {/if}
    </div>
  </div>

  <!-- Row 4: Manual dial -->
  <div class="dial-section">
    <h4>{$_("netmon_connect_to_peer")}</h4>
    <div class="dial-form">
      <input
        type="text"
        placeholder={$_("netmon_dial_peerid_ph")}
        bind:value={dialPeerId}
        class="dial-input peerid-input"
      />
      <input
        type="text"
        placeholder={$_("netmon_dial_addr_ph")}
        bind:value={dialAddr}
        class="dial-input addr-input"
      />
      <button class="dial-btn" onclick={handleDial} disabled={!dialPeerId.trim() || !dialAddr.trim() || dialing}>
        {dialing ? $_("netmon_connecting") : $_("netmon_connect")}
      </button>
    </div>
    {#if dialMessage}
      <div class="dial-message" class:success={dialMessageOk} class:error={!dialMessageOk}>{dialMessage}</div>
    {/if}
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

  .peerid-display {
    cursor: pointer;
    user-select: none;
  }

  .peerid-display:hover {
    color: #3b82f6;
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

  .dial-section {
    margin-top: 16px;
    padding-top: 16px;
    border-top: 1px solid var(--border-color, #2a2a2a);
  }

  .dial-section h4 {
    margin: 0 0 8px;
    font-size: 13px;
    font-weight: 600;
    color: var(--text-primary, #fafafa);
  }

  .dial-form {
    display: flex;
    gap: 6px;
    align-items: center;
  }

  .dial-input {
    background: var(--bg-secondary, #222);
    border: 1px solid var(--border-color, #2a2a2a);
    border-radius: 6px;
    padding: 6px 10px;
    color: var(--text-primary, #fafafa);
    font-size: 12px;
    font-family: monospace;
    outline: none;
    transition: border-color 0.2s;
  }

  .dial-input:focus {
    border-color: #3b82f6;
  }

  .peerid-input {
    flex: 2;
    min-width: 0;
  }

  .addr-input {
    flex: 3;
    min-width: 0;
  }

  .dial-btn {
    background: #3b82f6;
    border: none;
    border-radius: 6px;
    padding: 6px 14px;
    color: #fff;
    font-size: 12px;
    font-weight: 500;
    cursor: pointer;
    white-space: nowrap;
    transition: background 0.2s;
  }

  .dial-btn:hover:not(:disabled) {
    background: #2563eb;
  }

  .dial-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .dial-message {
    margin-top: 6px;
    font-size: 11px;
    font-family: monospace;
  }

  .dial-message.success {
    color: #22c55e;
  }

  .dial-message.error {
    color: #ef4444;
  }

  .loading-skeleton {
    display: flex;
    flex-direction: column;
    gap: 16px;
    padding: 12px 0;
  }

  .skeleton-row {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  .skeleton-item {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 100px;
  }

  .skeleton-label {
    font-size: 11px;
    color: var(--text-secondary, #9ca3af);
  }

  .skeleton-badge {
    height: 22px;
    width: 80px;
    border-radius: 4px;
    background: var(--border-color, #2a2a2a);
  }

  .skeleton-topology {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .skeleton-title {
    height: 16px;
    width: 140px;
    border-radius: 4px;
    background: var(--border-color, #2a2a2a);
  }

  .skeleton-canvas {
    height: 200px;
    border-radius: 6px;
    background: var(--border-color, #2a2a2a);
  }

  @keyframes pulse {
    0%, 100% { opacity: 0.4; }
    50% { opacity: 0.8; }
  }

  .pulse {
    animation: pulse 1.5s ease-in-out infinite;
  }
</style>