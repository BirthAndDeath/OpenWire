use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, AeadCore, KeyInit, OsRng},
};
use anyhow::Context;
use aws_lc_rs::kem::{DecapsulationKey, EncapsulationKey, ML_KEM_768};
use blake3;

/// ML-KEM-768 公钥大小（字节）
const MLKEM768_PUBLIC_KEY_SIZE: usize = 1184;
/// ML-KEM-768 私钥大小（字节）
const MLKEM768_SECRET_KEY_SIZE: usize = 2400;
/// ML-KEM-768 密文大小（字节）
const MLKEM768_CIPHERTEXT_SIZE: usize = 1088;

/// 当前加密协议版本
///
/// 此版本号在 encrypt_message() 中写入加密载荷的第一个字节，
/// 在 decrypt_message() 中检查并拒绝不匹配的版本。
///
/// 注意：此版本号仅用于加密协议（ML-KEM + AES-GCM）的兼容性管理，
/// 不用于 ChatMessage 结构体的序列化格式版本管理。
/// 若需支持消息序列化格式的向前兼容，应在 ChatMessage 结构体中
/// 添加独立的 version 字段。
const CURRENT_ENCRYPTION_VERSION: u8 = 1;

/// ML-KEM/Kyber 密钥对类型别名
pub type MlKemKeypair = (Vec<u8>, Vec<u8>); // (public_key, secret_key)

/// 生成新的 ML-KEM 密钥对（临时密钥交换）
///
/// # 设计说明
/// ML-KEM 密钥对是**临时**的，每次会话重新生成。
/// 持久化身份由 ML-DSA 公钥提供，ML-KEM 仅用于一次会话的密钥封装。
///
/// # 返回
/// - public_key: 公钥(用于加密)
/// - secret_key: 私钥(用于解密)
pub fn generate_mlkem_keypair() -> anyhow::Result<MlKemKeypair> {
    let decap_key =
        DecapsulationKey::generate(&ML_KEM_768).context("Failed to generate ML-KEM-768 keypair")?;

    let encap_key = decap_key
        .encapsulation_key()
        .context("Failed to get encapsulation key")?;

    let pk_bytes = encap_key
        .key_bytes()
        .context("Failed to serialize public key")?;
    let sk_bytes = decap_key
        .key_bytes()
        .context("Failed to serialize private key")?;

    Ok((pk_bytes.as_ref().to_vec(), sk_bytes.as_ref().to_vec()))
}

/// 将 ML-KEM 公钥序列化为字节数组
pub fn serialize_mlkem_public_key(pk: &[u8]) -> Vec<u8> {
    pk.to_vec()
}

/// 从字节数组反序列化 ML-KEM 公钥
pub fn deserialize_mlkem_public_key(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    if bytes.len() != MLKEM768_PUBLIC_KEY_SIZE {
        return Err(anyhow::anyhow!(
            "Invalid ML-KEM public key length: expected {}, got {}",
            MLKEM768_PUBLIC_KEY_SIZE,
            bytes.len()
        ));
    }
    Ok(bytes.to_vec())
}

/// 将 ML-KEM 私钥序列化为字节数组
pub fn serialize_mlkem_private_key(sk: &[u8]) -> Vec<u8> {
    sk.to_vec()
}

/// 从字节数组反序列化 ML-KEM 私钥
pub fn deserialize_mlkem_private_key(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    if bytes.len() != MLKEM768_SECRET_KEY_SIZE {
        return Err(anyhow::anyhow!(
            "Invalid ML-KEM private key length: expected {}, got {}",
            MLKEM768_SECRET_KEY_SIZE,
            bytes.len()
        ));
    }
    Ok(bytes.to_vec())
}

/// 使用 ML-KEM/Kyber + AES-GCM 进行混合加密
///
/// # 架构说明
/// - **ML-DSA 公钥**：持久化的身份标识，用于签名验证，唯一标识联系人
/// - **ML-KEM 公钥**：临时密钥交换密钥，每次会话重新生成，不持久化存储
/// - **临时 PeerID（Ed25519）**：仅用于传输层连接和路由，每次启动可变化
/// - **消息加密**：使用 ML-KEM 进行密钥封装，然后用 AES-GCM 加密消息
///
/// # 流程
/// 1. 使用接收方的 ML-KEM 公钥进行密钥封装（KEM）
/// 2. 得到共享密钥和封装的密文
/// 3. 从共享密钥派生 AES 密钥
/// 4. 使用 AES-GCM 加密实际消息数据
/// 5. 返回：encapsulated_key + nonce + ciphertext
///
/// # 优势
/// - **后量子安全**：ML-KEM/Kyber 是 NIST 标准化的后量子密钥封装机制
/// - **身份与加密分离**：ML-DSA 提供持久身份，ML-KEM 提供临时会话加密
/// - **前向保密**：每次加密使用随机性，相同的明文产生不同的密文
pub fn encrypt_message(data: &[u8], recipient_mlkem_public_key: &[u8]) -> anyhow::Result<Vec<u8>> {
    // 1. 验证公钥长度并反序列化
    let pk_bytes = deserialize_mlkem_public_key(recipient_mlkem_public_key)?;

    // 2. 使用 AWS LC RS 进行密钥封装(KEM)
    let encap_key = EncapsulationKey::new(&ML_KEM_768, &pk_bytes)
        .map_err(|e| anyhow::anyhow!("Invalid ML-KEM public key: {:?}", e))?;

    let (shared_secret, ciphertext_kem) = encap_key
        .encapsulate()
        .map_err(|e| anyhow::anyhow!("KEM encapsulation failed: {:?}", e))?;

    // 3. 从共享密钥派生 AES-256 密钥
    let aes_key = derive_aes_key_from_mlkem(shared_secret.as_ref());

    // 4. 生成随机 nonce
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);

    // 5. 使用 AES-GCM 加密数据
    let cipher = Aes256Gcm::new(&aes_key.into());
    let ciphertext = cipher
        .encrypt(&nonce, data)
        .map_err(|e| anyhow::anyhow!("AES-GCM encryption failed: {}", e))?;

    // 6. 组合:version + ciphertext_kem + nonce + ciphertext
    let kem_ciphertext_bytes = ciphertext_kem.as_ref();
    let mut result = Vec::with_capacity(
        1 + // version byte
        kem_ciphertext_bytes.len() +
        nonce.len() +
        ciphertext.len(),
    );

    // 添加版本标识
    result.push(CURRENT_ENCRYPTION_VERSION);
    result.extend_from_slice(kem_ciphertext_bytes);
    result.extend_from_slice(&nonce);
    result.extend_from_slice(&ciphertext);

    Ok(result)
}

/// 使用 ML-KEM/Kyber + AES-GCM 进行混合解密
///
/// # 流程
/// 1. 提取 Kyber 封装的密文
/// 2. 使用本地 Kyber 私钥进行解封装,得到共享密钥
/// 3. 从共享密钥派生 AES 密钥
/// 4. 使用 AES-GCM 解密数据
pub fn decrypt_message(
    encrypted_data: &[u8],
    local_mlkem_private_key: &[u8],
) -> anyhow::Result<Vec<u8>> {
    // 1. 验证私钥长度并反序列化
    let sk_bytes = deserialize_mlkem_private_key(local_mlkem_private_key)?;

    // 2. 检查版本标识并解析 KEM 封装的密文
    if encrypted_data.is_empty() {
        return Err(anyhow::anyhow!("Encrypted data is empty"));
    }

    let version = encrypted_data[0];
    if version != CURRENT_ENCRYPTION_VERSION {
        return Err(anyhow::anyhow!(
            "Unsupported encryption version: {}. Current version is {}",
            version,
            CURRENT_ENCRYPTION_VERSION
        ));
    }

    let data_without_version = &encrypted_data[1..];
    let kem_ciphertext_size = MLKEM768_CIPHERTEXT_SIZE;
    if data_without_version.len() < kem_ciphertext_size {
        return Err(anyhow::anyhow!(
            "Encrypted data too short to contain KEM ciphertext"
        ));
    }

    let (kem_ciphertext_bytes, rest) = data_without_version.split_at(kem_ciphertext_size);

    // 3. 使用私钥解封装,得到共享密钥
    let decap_key = DecapsulationKey::new(&ML_KEM_768, &sk_bytes)
        .map_err(|e| anyhow::anyhow!("Invalid ML-KEM private key: {:?}", e))?;

    // 将字节切片转换为 Ciphertext 类型
    let ciphertext: aws_lc_rs::kem::Ciphertext = kem_ciphertext_bytes.into();

    let shared_secret = decap_key
        .decapsulate(ciphertext)
        .map_err(|e| anyhow::anyhow!("KEM decapsulation failed: {:?}", e))?;

    // 4. 从共享密钥派生 AES-256 密钥
    let aes_key = derive_aes_key_from_mlkem(shared_secret.as_ref());

    // 5. 分离 nonce 和 ciphertext
    if rest.len() < 12 {
        return Err(anyhow::anyhow!("Encrypted data too short to contain nonce"));
    }
    let (nonce_bytes, ciphertext) = rest.split_at(12);
    let nonce = Nonce::from_slice(nonce_bytes);

    // 6. 使用 AES-GCM 解密
    let cipher = Aes256Gcm::new(&aes_key.into());
    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| anyhow::anyhow!("AES-GCM decryption failed: {}", e))?;

    Ok(plaintext)
}

// ========== 内部辅助函数 ==========

/// 从 ML-KEM 共享密钥派生 AES-256 密钥
///
/// 使用 BLAKE3 的 KDF 模式，添加上下文信息以提高安全性
fn derive_aes_key_from_mlkem(shared_secret: &[u8]) -> [u8; 32] {
    let mut key = [0u8; 32];
    let mut hasher = blake3::Hasher::new();

    // 添加上下文信息以防止密钥误用
    hasher.update(b"ChatCore-AES-256-Key-Derivation-v1");
    hasher.update(shared_secret);

    // 使用 XOF（可扩展输出函数）模式派生密钥
    hasher.finalize_xof().fill(&mut key);

    key
}

/// 验证公钥是否与 PeerID 匹配
///
/// libp2p 的 PeerID 就是从公钥计算得出的，所以这个验证非常可靠
pub fn verify_public_key_matches_peer_id(
    public_key: &[u8],
    peer_id: &libp2p::PeerId,
) -> anyhow::Result<bool> {
    // 从公钥重建 libp2p PublicKey
    let pk = libp2p::identity::PublicKey::try_decode_protobuf(public_key)?;
    let computed_peer_id = pk.to_peer_id();

    Ok(&computed_peer_id == peer_id)
}

/// 恒定时间比较两个字节数组
///
/// 防止时序攻击，无论比较结果如何，都执行相同数量的操作
pub fn constant_time_compare(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }

    let mut result = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        result |= x ^ y;
    }

    result == 0
}

/// 使用恒定时间比较验证两个字节数组是否相等
///
/// 适用于比较密钥、哈希值等敏感数据
pub fn verify_bytes_constant_time(a: &[u8], b: &[u8]) -> anyhow::Result<()> {
    if constant_time_compare(a, b) {
        Ok(())
    } else {
        Err(anyhow::anyhow!("Bytes do not match"))
    }
}
