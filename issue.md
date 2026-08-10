# 隐患/问题记录

## 严重

### 1. DiscoverPeer 中继响应无签名认证

`PeerInfoReceived`（中继 DiscoverPeer 响应）直接携带 `mldsa_pubkey_hex`、`peer_id`、`mlkem_pubkey_hex`，但无签名。中继被攻陷后可返回任意绑定关系，导致调用方信任错误的公钥→PeerID 映射。

**位置**：`openwire_core/src/core/handle/event_loop.rs:407-421`  
**影响**：机密性（若中继恶意可劫持消息）  
**已修复**：❌ 未修复

### 2. DHT GetProviders 结果无条件信任

`GetProvidersResult` 返回的 provider 列表被无条件缓存为 `pubkey→PeerID` 映射（`kademlia.rs:884-934`）。虽然 FriendOnline 已有签名验证，但 DHT 返回的虚假 provider 仍会被缓存，导致 UI 可能显示错误在线状态，且 `store.set_pubkey_peerid` 写入的映射可能影响后续查询。

**位置**：`openwire_core/src/actor/p2p/mod.rs:884-934` + `event_loop.rs:332-406`  
**影响**：可用性（UI 显示错误）、中等误导  
**已修复**：❌ 未修复  
**建议**：DHT provider 仅作为候选拨号地址，不直接作为身份映射；待 FriendOnline 验证通过后再写入。

### 3. Tombstone 是假的（重新发布而非删除）

`publish_tombstone_records` 实际调用 `start_providing` 重新发布 identity，与删除语义相反。

**位置**：`openwire_core/src/core/identity_ops/ops.rs:276-286`  
**影响**：已删除的身份仍可被 DHT 查询到，无法真正撤销  
**已修复**：❌ 未修复  
**建议**：应调用 `stop_providing` 停止提供，并发布一个签名过的撤销声明（tombstone record）。

### 4. `incoming_message` 的 `sender_mldsa_pubkey_hex` 未验证与连接身份的绑定

`events.rs:37-38` 从 `request.sender_public_key` 提取 pubkey，虽然该值被 ML-DSA 签名覆盖（`verify()` 验证签名一致性），但签名只证明"消息由持有该私钥的人签名"，未证明"该公钥是当前连接 PeerID 对应的身份"。攻击者可用自己的密钥对签名消息，持有效签名，但声称是其他身份——攻击者自己的公钥不一定是联系人，所以 `is_known_contact` 会拒绝；但若攻击者先通过某种方式让受害者添加了攻击者的公钥为联系人，则攻击者可以用自己的密钥对签名、冒充另一个身份发送消息。

**位置**：`openwire_core/src/p2p/events.rs:37-38`  
**影响**：机密性/完整性（需要攻击者已是联系人）  
**已修复**：❌ 未修复  
**建议**：在 `handle_incoming_request` 中验证 `sender_mldsa_pubkey_hex` 与 `peer` 在 `DhtCache` 中是否有一致的映射，或者验证 FriendOnline 签名已为该 `peer` 绑定该 `pubkey`。

---

## 中等

### 5. `rootcell` 中 `save_encrypted_private_key` 的 `data_dir` 路径校验不足

已移除 `canonicalize` 检查，但 `data_dir` 来自不可信来源时仍可能指向意外路径。当前 `data_dir` 来自配置，在可信范围内。

**位置**：`rootcell/src/identity.rs:225-226`  
**已修复**：✅ 已修复（移除 canonicalize，改为无条件 create_dir_all）

### 6. `openwire_core` 中 `delete_identity` 删除 `_mlkem` 加密文件是死代码

`storage/identity.rs:80-83` 同时删除 `{}_mldsa` 和 `{}_mlkem`，但 `generate_complete_identity` 从未保存过 `_mlkem` 密钥文件。

**位置**：`openwire_core/src/storage/identity.rs:76-83`  
**已修复**：❌ 未修复  
**建议**：移除 `_mlkem` 删除行，或如果将来 ML-KEM 私钥需要持久化则改为实际保存。

### 7. `try_init` 中 Master Key 被重复加载

`load_or_generate_complete_identity` 内部已通过 `rootcell::identity::PrivateKeyHandle::save`/`load` 访问 keyring，之后 `mod.rs:158` 又显式 `load` 一次，两次触发 keyring 操作。

**位置**：`openwire_core/src/core/mod.rs:142-167`  
**已修复**：❌ 未修复  
**建议**：`load_or_generate_complete_identity` 返回缓存的私钥字节，避免重复 keyring 访问。

### 8. `aws_lc_rs::unstable::*` 弃用警告

`aws-lc-rs` 已稳定化 ML-DSA 类型，但项目仍使用 `unstable` 路径。

**位置**：`openwire_core/src/signature.rs:3`、`openwire_core/src/identity.rs:176-178`  
**已修复**：❌ 未修复（项目决策：待上游稳定后再切，记住移除 unstable 前缀）

---

## 轻微

### 9. `cleanup_expired_dht_records` 清理粒度不够

每小时清理一次，但只清理不在 `connected_peers` 中的条目。如果某 peer 断连后短时间内重连，缓存会被清理再重建，浪费。

**位置**：`openwire_core/src/core/handle/event_loop.rs:513-556`  
**建议**：增加 TTL 或 LRU 淘汰策略，而非仅依赖连接状态。

### 10. `validate_mldsa_pubkey_hex` 不是真正的密码学验证

只检查 hex 编码和长度，不验证公钥曲线有效性。注释已说明待 aws-lc-rs 稳定后补充。

**位置**：`openwire_core/src/signature.rs:23-48`  
**影响**：低（验证时机在 `verify_signature` 调用时，无效公钥会在那时被拒绝）