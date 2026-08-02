use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit},
};
use aws_lc_rs::kem::{DecapsulationKey, EncapsulationKey, ML_KEM_768};
use blake3;

/// ML-KEM-768 公钥大小（字节）
///
/// 注意：aws-lc-rs 的 ML_KEM_768 实际公钥大小为 1184 字节。
/// 此常量用于序列化/反序列化时的容量预分配，不做严格校验。
pub const MLKEM768_PUBLIC_KEY_SIZE: usize = 1184;
/// ML-KEM-768 私钥大小（字节）
///
/// 注意：aws-lc-rs 的 ML_KEM_768 实际私钥大小为 2400 字节。
/// 此常量用于序列化/反序列化时的容量预分配，不做严格校验。
pub const MLKEM768_SECRET_KEY_SIZE: usize = 2400;
/// ML-KEM-768 密文大小（字节）
///
/// aws-lc-rs 的 ML_KEM_768 encapsulate() 返回标准 ML-KEM-768 的
/// ciphertext，大小为 1088 字节。
pub const MLKEM768_CIPHERTEXT_SIZE: usize = 1088;

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
/// 5. 返回：version + encapsulated_key + nonce + ciphertext
///
/// # 优势
/// - **后量子安全**：ML-KEM/Kyber 是 NIST 标准化的后量子密钥封装机制
/// - **身份与加密分离**：ML-DSA 提供持久身份，ML-KEM 提供临时会话加密
/// - **前向保密**：每次加密使用随机性，相同的明文产生不同的密文
pub fn encrypt_message(
    data: &[u8],
    recipient_mlkem_public_key: &[u8],
) -> crate::error::CryptoResult<Vec<u8>> {
    // 1. 使用 AWS LC RS 进行密钥封装(KEM)
    let encap_key = EncapsulationKey::new(&ML_KEM_768, recipient_mlkem_public_key)
        .map_err(crate::error::CryptoError::InvalidMlKemPublicKey)?;

    let (ciphertext_kem, shared_secret) = encap_key
        .encapsulate()
        .map_err(crate::error::CryptoError::KemEncapsulationFailed)?;

    // 2. 从共享密钥派生 AES-256 密钥
    let aes_key = derive_aes_key_from_mlkem(shared_secret.as_ref());

    // 3. 生成随机 nonce
    let nonce_bytes: [u8; 12] = rand::random();
    #[allow(deprecated)]
    let nonce = Nonce::from_slice(&nonce_bytes);

    // 4. 使用 AES-GCM 加密数据
    let cipher = Aes256Gcm::new(&aes_key.into());
    let ciphertext = cipher
        .encrypt(nonce, data)
        .map_err(|e| crate::error::CryptoError::AesGcmEncryptionFailed(e.to_string()))?;

    // 5. 组合: version + ciphertext_kem + nonce + ciphertext
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
    result.extend_from_slice(nonce);
    result.extend_from_slice(&ciphertext);

    Ok(result)
}

/// 使用 ML-KEM/Kyber + AES-GCM 进行混合解密
///
/// # 参数
/// - `encrypted_data`: 加密数据（version + kem_ciphertext + nonce + aes_ciphertext）
/// - `decap_key`: ML-KEM 解封装密钥对象（由 ChatCore 缓存，避免序列化/反序列化问题）
///
/// # 设计说明
/// 注意：此函数接受 `&DecapsulationKey` 而非 `&[u8]` 私钥字节。
/// 这是因为 aws-lc-rs 的 `DecapsulationKey::key_bytes()` 输出格式与
/// `DecapsulationKey::new()` 输入格式不兼容（已知的库限制），
/// 因此无法通过序列化/反序列化私钥字节来重建 DecapsulationKey。
/// 解决方案是在 ChatCore 中缓存 DecapsulationKey 对象，直接传入引用。
///
/// # 流程
/// 1. 提取 Kyber 封装的密文
/// 2. 使用本地 Kyber 私钥进行解封装,得到共享密钥
/// 3. 从共享密钥派生 AES 密钥
/// 4. 使用 AES-GCM 解密数据
pub fn decrypt_message(
    encrypted_data: &[u8],
    decap_key: &DecapsulationKey,
) -> crate::error::CryptoResult<Vec<u8>> {
    // 1. 检查版本标识
    if encrypted_data.is_empty() {
        return Err(crate::error::CryptoError::EncryptedDataEmpty);
    }

    let version = encrypted_data[0];
    if version != CURRENT_ENCRYPTION_VERSION {
        return Err(crate::error::CryptoError::UnsupportedEncryptionVersion {
            version,
            current: CURRENT_ENCRYPTION_VERSION,
        });
    }

    let data_without_version = &encrypted_data[1..];

    // 2. 提取 KEM 封装的密文（固定大小）
    if data_without_version.len() < MLKEM768_CIPHERTEXT_SIZE {
        return Err(crate::error::CryptoError::EncryptedDataTooShort);
    }

    let (kem_ciphertext_bytes, rest) = data_without_version.split_at(MLKEM768_CIPHERTEXT_SIZE);

    // 3. 使用私钥解封装,得到共享密钥
    // 将字节切片转换为 Ciphertext 类型
    let ciphertext: aws_lc_rs::kem::Ciphertext = kem_ciphertext_bytes.into();

    let shared_secret = decap_key
        .decapsulate(ciphertext)
        .map_err(crate::error::CryptoError::KemDecapsulationFailed)?;

    // 4. 从共享密钥派生 AES-256 密钥
    let aes_key = derive_aes_key_from_mlkem(shared_secret.as_ref());

    // 5. 分离 nonce 和 ciphertext
    if rest.len() < 12 {
        return Err(crate::error::CryptoError::EncryptedDataTooShort);
    }
    let (nonce_bytes, ciphertext) = rest.split_at(12);
    #[allow(deprecated)]
    let nonce = Nonce::from_slice(nonce_bytes);

    // 6. 使用 AES-GCM 解密
    let cipher = Aes256Gcm::new(&aes_key.into());
    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| crate::error::CryptoError::AesGcmDecryptionFailed(e.to_string()))?;

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

/// 恒定时间比较两个字节数组
///
/// 防止时序攻击，无论比较结果如何，都执行相同数量的操作。
/// 使用 `subtle` crate 确保编译器不会优化掉恒定时间特性。
pub fn constant_time_compare(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.ct_eq(b).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mlkem_encrypt_decrypt_roundtrip() {
        let decap_key =
            DecapsulationKey::generate(&ML_KEM_768).expect("生成 DecapsulationKey 失败");
        let encap_key = decap_key
            .encapsulation_key()
            .expect("获取 EncapsulationKey 失败");
        let pk_bytes = encap_key.key_bytes().expect("获取公钥字节失败");

        let plaintext = b"Hello, ML-KEM encryption!";
        let encrypted = encrypt_message(plaintext, pk_bytes.as_ref()).unwrap();

        println!("加密输出总大小: {} 字节", encrypted.len());
        println!("  版本: {} 字节", 1);
        println!("  KEM 密文: {} 字节", MLKEM768_CIPHERTEXT_SIZE);
        println!("  Nonce: {} 字节", 12);
        println!(
            "  AES-GCM 密文: {} 字节",
            encrypted.len() - 1 - MLKEM768_CIPHERTEXT_SIZE - 12
        );

        assert_eq!(encrypted[0], CURRENT_ENCRYPTION_VERSION, "版本字节应为 1");

        let decrypted = decrypt_message(&encrypted, &decap_key).unwrap();
        assert_eq!(decrypted, plaintext, "解密后的数据应与原始明文一致");
    }

    #[test]
    fn test_mlkem_encrypt_short_data() {
        let decap_key =
            DecapsulationKey::generate(&ML_KEM_768).expect("生成 DecapsulationKey 失败");
        let encap_key = decap_key
            .encapsulation_key()
            .expect("获取 EncapsulationKey 失败");
        let pk_bytes = encap_key.key_bytes().expect("获取公钥字节失败");

        let plaintext = b"ss";
        let encrypted = encrypt_message(plaintext, pk_bytes.as_ref()).unwrap();

        println!("短数据加密后总大小: {} 字节", encrypted.len());
        println!("  KEM 密文: {} 字节", MLKEM768_CIPHERTEXT_SIZE);
        println!(
            "  AES-GCM 密文: {} 字节",
            encrypted.len() - 1 - MLKEM768_CIPHERTEXT_SIZE - 12
        );

        let decrypted = decrypt_message(&encrypted, &decap_key).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_mlkem_wrong_key_decrypt_fails() {
        let decap_key =
            DecapsulationKey::generate(&ML_KEM_768).expect("生成 DecapsulationKey 失败");
        let encap_key = decap_key
            .encapsulation_key()
            .expect("获取 EncapsulationKey 失败");
        let pk_bytes = encap_key.key_bytes().expect("获取公钥字节失败");

        let wrong_decap_key =
            DecapsulationKey::generate(&ML_KEM_768).expect("生成错误的 DecapsulationKey 失败");

        let plaintext = b"secret message";
        let encrypted = encrypt_message(plaintext, pk_bytes.as_ref()).unwrap();

        let result = decrypt_message(&encrypted, &wrong_decap_key);
        assert!(result.is_err(), "使用错误的私钥解密应该失败");
    }

    #[test]
    fn test_mlkem_tampered_ciphertext_fails() {
        let decap_key =
            DecapsulationKey::generate(&ML_KEM_768).expect("生成 DecapsulationKey 失败");
        let encap_key = decap_key
            .encapsulation_key()
            .expect("获取 EncapsulationKey 失败");
        let pk_bytes = encap_key.key_bytes().expect("获取公钥字节失败");

        let plaintext = b"test data";
        let mut encrypted = encrypt_message(plaintext, pk_bytes.as_ref()).unwrap();

        let aes_start = 1 + MLKEM768_CIPHERTEXT_SIZE + 12;
        if encrypted.len() > aes_start {
            encrypted[aes_start] ^= 0xFF;
        }

        let result = decrypt_message(&encrypted, &decap_key);
        assert!(result.is_err(), "篡改后的密文解密应该失败");
    }

    #[test]
    fn test_mlkem_multiple_encryptions_different() {
        let decap_key =
            DecapsulationKey::generate(&ML_KEM_768).expect("生成 DecapsulationKey 失败");
        let encap_key = decap_key
            .encapsulation_key()
            .expect("获取 EncapsulationKey 失败");
        let pk_bytes = encap_key.key_bytes().expect("获取公钥字节失败");

        let plaintext = b"same data";
        let encrypted1 = encrypt_message(plaintext, pk_bytes.as_ref()).unwrap();
        let encrypted2 = encrypt_message(plaintext, pk_bytes.as_ref()).unwrap();

        assert_ne!(encrypted1, encrypted2, "两次加密结果应该不同");

        assert_eq!(
            decrypt_message(&encrypted1, &decap_key).unwrap(),
            decrypt_message(&encrypted2, &decap_key).unwrap()
        );
    }
}
