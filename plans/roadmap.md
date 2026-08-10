# OpenWire Development Roadmap

## Phase 0: Foundation (Completed)

- [x] Post-quantum crypto (ML-DSA-65 + ML-KEM-768 + AES-GCM)
- [x] libp2p P2P networking (TCP, QUIC, WebSocket, relay)
- [x] Identity management (keyring + encrypted file storage)
- [x] Basic messaging (send/receive text, delivery receipts)
- [x] File transfer (hash sharing, chunked streaming, resume)
- [x] DHT discovery (Kademlia providers)
- [x] Relay-based discovery (DiscoverPeer protocol)
- [x] 3 frontends: Tauri desktop, CLI/TUI, server relay

## Phase 1: Security Hardening (Completed)

- [x] S1: send_file path traversal fix
- [x] S2: validate_contact fail-open fix
- [x] S3: delete_message ownership check
- [x] S4: save_nodes_config multiaddr validation
- [x] S5: log.rs expect → error propagation
- [x] S6: validate_mldsa_pubkey_hex length check
- [x] S7: DHT identity binding verification
- [x] S8: DHT ML-KEM record publisher
- [x] S9: Message replay protection (seen_hashes)
- [x] S10: Master key HKDF derivation
- [x] S11: Master key Zeroizing
- [x] S12: data_dir canonicalization
- [x] S13: delete_master_key error handling

## Phase 2: Architecture Simplification (Completed)

- [x] Remove generic Actor trait (405 lines → 35)
- [x] Remove download_actor.rs (139 lines)
- [x] Simplify P2pActorHandle (direct P2pCommand channel)
- [x] Split P2pActor: relay_handler.rs extracted

## Phase 3: Bug Fixes

- [x] F1: send_response channel drop → send error response
- [x] F2: Frontend file share format mismatch → detect FILE_SHARE prefix (legacy messages)
- [x] F3: bootstrap_nodes empty → load from config (P2pActorBuilder + graceful shutdown)
- [x] F4: delete_contact dialog timeout → 30s timeout
- [x] F5: delete_contact error propagation → use ?
- [x] F6: Identity deletion cleanup → clear peerid_to_pubkey
- [x] F7: mark_sent_by_hash error → handle gracefully
- [x] F8: DHT publish failure → send Warning event to UI
- [x] F9: Contact lastMsg wrong key → remove selectedId
- [x] F10: handleScroll parameter type → fix VList callback
- [x] F11: CLI status bar overlap → use bottom area
- [x] F12: CLI detect_file_share rfind → find

## Phase 4: Multi-Device Support (Current Design)

- [ ] P0: Add devices table + message_sync_status table
- [ ] P1: Extend DHT cache 1:1 → 1:N mapping
- [ ] P2: Extend NetEvent with SyncMessage/DeviceAnnounce
- [ ] P3: Device registration on startup (DeviceAnnounce broadcast)
- [ ] P4: Random device selection for message delivery
- [ ] P5: Device-to-device sync via SyncMessage
- [ ] P6: Deduplication and conflict resolution
- [ ] P7: Optional ML-KEM key distribution across devices

## Phase 5: Post-Quantum libp2p Standardization (Future Architecture Shift)

### Prerequisite

libp2p adds native post-quantum cryptographic support (2026+):

- `/噪点/noise/qp1` or equivalent QPC handshake protocol
- Kademlia with QPC-signed records
- Standardized multiaddr formats for QPC PeerIDs

### Current Architecture Pain Points

```
DHT: ML-DSA pubkey → PeerID     (1:1 mapping, overwrites on update)
DHT: PeerID → ML-KEM pubkey      (separate lookup, unauthenticated record)
Problem: Two lookups, no peer-to-peer key exchange, sender responsible for all devices
```

### Future Architecture

```
DHT: PeerID → Multiaddr[ ]       (1:N, standard libp2p)
Transport: Random protocol selected for each connection
Protocol: Noise_XK + QPC hybrid handshake
  → ML-KEM encapsulated session key sent inline with handshake
  → ML-DSA signed handshake messages for identity binding
Result: No separate ML-KEM lookup needed
```

### Key Changes

#### 1. DHT Simplification

- **Current**: `pubkey → peerid` + `peerid → mlkem_pubkey` (two maps, two lookups)
- **Future**: `peerid → multiaddrs` (standard Kademlia, one lookup)
- Remove `DhtCache::mlkem_pubkeys` entirely
- Remove `DhtCache::pubkey_peerid` (replaced by Kademlia provider records)

#### 2. On-the-fly Key Exchange

- **Current**: Look up ML-KEM key from DHT, then encrypt
- **Future**: Connect to peer, negotiate QPC session key via Noise handshake
  - ML-KEM-768 encapsulated in Noise handshake payload
  - ML-DSA-65 signs the handshake to bind identity
  - Session key derived from both ML-KEM shared secret + Noise ephemeral
- No separate ML-KEM key storage or lookup needed

#### 3. Message Delivery (Sender → One Device)

- **Current**: Sender responsible for looking up PeerID + ML-KEM key
- **Future**: Sender queries DHT for PeerID's multiaddrs, picks address randomly
  - Connects via selected protocol (TCP/QUIC/WebSocket)
  - QPC handshake verifies recipient identity + establishes session key
  - Sends message over encrypted channel
  - Sender is DONE — responsible for ONE delivery only

#### 4. Multi-Device Sync (Recipient Device → Other Devices)

- **Current**: No multi-device support
- **Future**: Receiving device handles sync to its other devices:
  1. Device A receives message
  2. Device A queries its own DHT for other devices of same identity
  3. Device A connects to Device B/C via QPC handshake
  4. Device A sends `SyncMessage` via NetEvent (ML-DSA signed)
  5. Device B/C verify signature, decrypt, store locally
  6. Device B/C send Ack back

#### 5. NetEvent Expansion

```
NetEventRequest::SyncMessage {
    sender_mldsa_pubkey_hex: String,  // original sender
    encrypted_data: Vec<u8>,          // ML-KEM encapsulated for target device
    msgtype: u8,
    original_hash: [u8; 32],          // dedup
    timestamp: u64,
    signature: Vec<u8>,               // ML-DSA signed by syncing device
}
NetEventRequest::DeviceAnnounce {
    mldsa_pubkey_hex: String,
    peer_id: String,
    listen_addrs: Vec<String>,
    mlkem_pubkey_hex: String,
    device_name: String,
    signature: Vec<u8>,               // ML-DSA signed by identity private key
}
```

### Migration Path

| Step | What | Risk |
| ------ | ------ | ------ |
| 1 | libp2p releases QPC Noise handshake | Wait for upstream |
| 2 | Add Noise_XK + QPC as a swarm transport protocol | Backward compat with existing Noise |
| 3 | Remove ML-KEM DHT records, rely on inline key exchange | Breaks old clients |
| 4 | Switch DHT to standard PeerID → multiaddr | Simplifies code |
| 5 | Implement DeviceAnnounce + SyncMessage | New feature |
| 6 | Remove old DHT cache abstraction | Cleanup |

### Backward Compatibility

- Old clients use old protocol (pubkey → peerid → mlkem lookup)
- New clients prefer QPC handshake, fall back to old two-step lookup
- Messages are ML-DSA signed regardless of transport layer
- DHT records can be dual-published during transition period

## Phase 6: Quality & Polish (Future)

- [ ] Unit tests for filename sanitization
- [ ] Regression tests for error handling paths
- [ ] Integration tests for DHT discovery
- [ ] Android CI build verification
- [ ] Diagnostics and monitoring improvements
- [ ] Performance profiling and optimization
- [ ] UX improvements (loading states, error messages)

## Key Design Decisions

### Why P2P Actor Pattern?

libp2p's Swarm requires a single-threaded `poll()` event loop that cannot be shared.
P2pActor owns the Swarm and runs the event loop, while ChatCore handles business logic.
Communication via mpsc channels avoids locks and deadlocks.

### Why Not Use the Generic Actor Trait?

The generic Actor trait was designed for simple command-processing actors.
P2pActor needs `tokio::select!` between Swarm events and command channels,
which the generic loop couldn't support. Only one actor (DownloadResponseActor)
used the generic trait, and it was trivial enough to inline.

### Why HKDF for Key Derivation?

The master key (32 bytes) from the keyring was used directly as AES-256-GCM key.
HKDF provides domain separation: each identity gets a unique derived key,
so compromising one identity's ciphertext doesn't affect others.

### Why Relay-Based Discovery (DiscoverPeer)?

Kademlia DHT discovery fails when routing table is empty (bootstrap nodes
use incompatible `/ipfs/kad/1.0.0` protocol). DiscoverPeer uses relay nodes
as trusted intermediaries: relay nodes cache FriendOnline identity mappings
and respond to discovery queries from their DHT cache.

### Why Not Server-Based Routing?

OpenWire is designed as truly P2P. Relay nodes are optional intermediaries
for NAT traversal, not central servers. The DiscoverPeer protocol is a
fallback for when DHT discovery fails, not a primary routing mechanism.
dht查询好友逻辑可以进行更改，根据好友公钥sha256进行查询，减少信息暴露和流量消耗
