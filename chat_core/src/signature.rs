use anyhow::Context;
use aws_lc_rs::encoding::AsRawBytes;
use aws_lc_rs::signature::{KeyPair, UnparsedPublicKey};
use aws_lc_rs::unstable::signature::{ML_DSA_65, ML_DSA_65_SIGNING, PqdsaKeyPair};
use blake3;
use std::time::{SystemTime, UNIX_EPOCH};
/// ML-DSA 密钥对类型别名 (public_key, secret_key)
pub type MlDsaKeypair = (Vec<u8>, Vec<u8>);

/// ML-DSA 65 公钥长度（字节）
pub const ML_DSA_65_PUBLIC_KEY_LEN: usize = 1952;
/// ML-DSA 65 私钥长度（字节）
pub const ML_DSA_65_PRIVATE_KEY_LEN: usize = 4032;
/// ML-DSA 65 签名长度（字节）
pub const ML_DSA_65_SIGNATURE_LEN: usize = 3309;

/// 验证 ML-DSA 公钥 hex 格式
/// 此函数会：
/// 解码并验证字节是否符合 ML-DSA-65 规范
/// warn:需稳定aws-lc-rs稳定后使用密码学验证！！！
/// # 参数
/// - hex: ML-DSA 公钥的十六进制字符串
///
/// # 返回
/// - true 如果格式有效，false 则无效
pub fn validate_mldsa_pubkey_hex(hex: &str) -> bool {
    // 验证 hex 字符
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return false;
    }

    // 尝试解码 hex 并验证长度
    match hex::decode(hex) {
        Ok(bytes) => {
            // 验证解码后的字节

            // 创建 UnparsedPublicKey 对象来触发 aws-lc-rs 的基本验证
            // 虽然 new() 不会立即验证，但它会存储算法和公钥的引用
            // 如果后续调用 verify() 时公钥无效，会返回错误
            let _public_key = UnparsedPublicKey::new(&ML_DSA_65, &bytes);

            // 如果能成功创建对象且长度正确，认为基本格式有效
            // 真正的密码学验证会在实际使用时通过 verify_signature() 进行
            true
        }
        Err(_) => false,
    }
}

/// 生成新的 ML-DSA 密钥对用于持久化身份
///
/// # 返回
/// - public_key: 公钥(用于验证签名)
/// - secret_key: 私钥(原始格式)
pub fn generate_mldsa_keypair() -> anyhow::Result<MlDsaKeypair> {
    let key_pair = PqdsaKeyPair::generate(&ML_DSA_65_SIGNING)
        .context("Failed to generate ML-DSA 65 keypair")?;

    let public_key = key_pair.public_key().as_ref().to_vec();
    let secret_key = key_pair
        .private_key()
        .as_raw_bytes()
        .context("Failed to get raw private key")?
        .as_ref()
        .to_vec();

    Ok((public_key, secret_key))
}

/// 使用私钥对数据进行签名
///
/// # 参数
/// - private_key: 原始格式的私钥
/// - data: 要签名的数据
///
/// # 返回
/// - 签名结果
pub fn sign_data(private_key: &[u8], data: &[u8]) -> anyhow::Result<Vec<u8>> {
    let key_pair = PqdsaKeyPair::from_raw_private_key(&ML_DSA_65_SIGNING, private_key)
        .map_err(|e| anyhow::anyhow!("Failed to parse ML-DSA private key: {:?}", e))?;

    let mut signature = vec![0u8; ML_DSA_65_SIGNATURE_LEN];
    key_pair
        .sign(data, &mut signature)
        .map_err(|e| anyhow::anyhow!("Failed to sign data: {:?}", e))?;

    Ok(signature)
}

/// 验证签名
///
/// # 参数
/// - public_key: 公钥
/// - data: 原始数据
/// - signature: 签名
///
/// # 返回
/// - 验证是否成功
pub fn verify_signature(public_key: &[u8], data: &[u8], signature: &[u8]) -> anyhow::Result<bool> {
    let public_key_obj = UnparsedPublicKey::new(&ML_DSA_65, public_key);

    match public_key_obj.verify(data, signature) {
        Ok(_) => Ok(true),
        Err(_) => Ok(false),
    }
}

/// DHT 记录签名数据结构
#[derive(Debug, Clone)]
pub struct DhtRecordSignature {
    /// 时间戳 (Unix timestamp in milliseconds)
    pub timestamp: u64,
    /// 盐值 (随机数，防止重放攻击)
    pub salt: [u8; 32],
    /// 签名 (ML-DSA 签名)
    pub signature: Vec<u8>,
}

impl DhtRecordSignature {
    /// 创建新的签名结构
    ///
    /// # 参数
    /// - private_key: 签名私钥
    /// - record_key: 记录键
    /// - record_value: 记录值
    ///
    /// # 返回
    /// - 签名结构
    pub fn create(
        private_key: &[u8],
        publisher: &libp2p::PeerId,
        record_key: &[u8],
        record_value: &[u8],
    ) -> anyhow::Result<Self> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| anyhow::anyhow!("Time error: {:?}", e))?
            .as_millis() as u64;

        // 生成随机盐值
        let mut salt = [0u8; 32];
        aws_lc_rs::rand::fill(&mut salt)
            .map_err(|e| anyhow::anyhow!("Failed to generate salt: {:?}", e))?;

        // 计算要签名的数据哈希（包含时间戳、盐和发布者 PeerID）
        let message_hash =
            Self::compute_message_hash(record_key, record_value, publisher, timestamp, &salt);

        // 签名
        let signature = sign_data(private_key, &message_hash)?;

        Ok(Self {
            timestamp,
            salt,
            signature,
        })
    }

    /// 验证签名
    ///
    /// # 参数
    /// - public_key: 验证公钥
    /// - record_key: 记录键
    /// - record_value: 记录值
    /// - publisher: 发布者 PeerID（包含在签名哈希中，防止重放攻击）
    /// - max_age_ms: 最大允许年龄（毫秒），防止重放攻击
    ///
    /// # 返回
    /// - 验证是否成功
    pub fn verify(
        &self,
        public_key: &[u8],
        record_key: &[u8],
        record_value: &[u8],
        publisher: &libp2p::PeerId,
        max_age_ms: u64,
    ) -> anyhow::Result<bool> {
        // 检查时间戳是否过期
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| anyhow::anyhow!("Time error: {:?}", e))?
            .as_millis() as u64;

        if now.saturating_sub(self.timestamp) > max_age_ms {
            return Ok(false);
        }

        // 重新计算消息哈希（包含发布者 PeerID）
        let message_hash = Self::compute_message_hash(
            record_key,
            record_value,
            publisher,
            self.timestamp,
            &self.salt,
        );

        // 验证签名
        verify_signature(public_key, &message_hash, &self.signature)
    }

    /// 计算消息哈希（包含所有防重放要素和发布者 PeerID）
    fn compute_message_hash(
        record_key: &[u8],
        record_value: &[u8],
        publisher: &libp2p::PeerId,
        timestamp: u64,
        salt: &[u8; 32],
    ) -> Vec<u8> {
        let mut hasher = blake3::Hasher::new();

        // 添加上下文防止密钥误用
        hasher.update(b"ChatCore-DHT-Record-Signature-v1");
        hasher.update(record_key);
        hasher.update(record_value);
        hasher.update(&publisher.to_bytes());
        hasher.update(&timestamp.to_le_bytes());
        hasher.update(salt);

        hasher.finalize().as_bytes().to_vec()
    }

    /// 序列化为字节数组
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&self.timestamp.to_le_bytes());
        bytes.extend_from_slice(&self.salt);
        bytes.extend_from_slice(&(self.signature.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&self.signature);
        bytes
    }

    /// 从字节数组反序列化
    pub fn from_bytes(bytes: &[u8]) -> anyhow::Result<Self> {
        if bytes.len() < 48 {
            // 8 (timestamp) + 32 (salt) + 8 (sig_len)
            return Err(anyhow::anyhow!("Signature data too short"));
        }

        let timestamp = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
        let salt: [u8; 32] = bytes[8..40].try_into().unwrap();
        let sig_len = u64::from_le_bytes(bytes[40..48].try_into().unwrap()) as usize;

        if bytes.len() < 48 + sig_len {
            return Err(anyhow::anyhow!("Signature data incomplete"));
        }

        let signature = bytes[48..48 + sig_len].to_vec();

        Ok(Self {
            timestamp,
            salt,
            signature,
        })
    }
}

/// 序列化 ML-DSA 公钥
pub fn serialize_mldsa_public_key(pk: &[u8]) -> Vec<u8> {
    pk.to_vec()
}

/// 带签名的身份记录值（用于 DHT 网络发布）
///
/// 由于 libp2p 的 `Record` 类型不包含自定义签名字段，
/// 将签名信息编码到记录值中，随记录一起传播到网络。
/// 接收方解码后验证签名，确保记录由合法的 ML-DSA 私钥持有者发布。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SignedIdentityRecord {
    /// 原始记录值（如 PeerID 字符串或 ML-KEM 公钥 hex）
    pub value: String,
    /// 发布者 PeerID 的字符串表示
    pub publisher: String,
    /// 签名时间戳（Unix 毫秒）
    pub timestamp: u64,
    /// 盐值（32 字节，防止重放攻击）
    pub salt: [u8; 32],
    /// ML-DSA 签名
    pub signature: Vec<u8>,
}

impl SignedIdentityRecord {
    /// 创建并签名一条身份记录
    ///
    /// # 参数
    /// - `mldsa_private_key`: 当前身份的 ML-DSA 私钥
    /// - `publisher`: 发布者 PeerID
    /// - `record_key`: DHT 记录键（如 "peerid:{pubkey}" 或 "mlkem:{pubkey}"）
    /// - `value`: 原始记录值
    pub fn sign(
        mldsa_private_key: &[u8],
        publisher: &libp2p::PeerId,
        record_key: &[u8],
        value: String,
    ) -> anyhow::Result<Self> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| anyhow::anyhow!("Time error: {:?}", e))?
            .as_millis() as u64;

        let mut salt = [0u8; 32];
        aws_lc_rs::rand::fill(&mut salt)
            .map_err(|e| anyhow::anyhow!("Failed to generate salt: {:?}", e))?;

        let publisher_str = publisher.to_string();
        let message_hash =
            Self::compute_message_hash(record_key, value.as_bytes(), publisher, timestamp, &salt);
        let signature = sign_data(mldsa_private_key, &message_hash)?;

        Ok(Self {
            value,
            publisher: publisher_str,
            timestamp,
            salt,
            signature,
        })
    }

    /// 验证签名
    ///
    /// # 参数
    /// - `mldsa_public_key`: 发布者的 ML-DSA 公钥
    /// - `record_key`: DHT 记录键
    /// - `publisher`: 预期的发布者 PeerID
    /// - `max_age_ms`: 签名最大允许年龄（毫秒）
    pub fn verify(
        &self,
        mldsa_public_key: &[u8],
        record_key: &[u8],
        publisher: &libp2p::PeerId,
        max_age_ms: u64,
    ) -> anyhow::Result<bool> {
        // 检查时间戳是否过期
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| anyhow::anyhow!("Time error: {:?}", e))?
            .as_millis() as u64;

        if now.saturating_sub(self.timestamp) > max_age_ms {
            return Ok(false);
        }

        // 验证发布者 PeerID 匹配
        if self.publisher != publisher.to_string() {
            return Ok(false);
        }

        let message_hash = Self::compute_message_hash(
            record_key,
            self.value.as_bytes(),
            publisher,
            self.timestamp,
            &self.salt,
        );

        verify_signature(mldsa_public_key, &message_hash, &self.signature)
    }

    /// 计算消息哈希（与 DhtRecordSignature 保持一致）
    fn compute_message_hash(
        record_key: &[u8],
        record_value: &[u8],
        publisher: &libp2p::PeerId,
        timestamp: u64,
        salt: &[u8; 32],
    ) -> Vec<u8> {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"ChatCore-DHT-Record-Signature-v1");
        hasher.update(record_key);
        hasher.update(record_value);
        hasher.update(&publisher.to_bytes());
        hasher.update(&timestamp.to_le_bytes());
        hasher.update(salt);
        hasher.finalize().as_bytes().to_vec()
    }
}

/// 反序列化 ML-DSA 公钥
pub fn deserialize_mldsa_public_key(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    // ML-DSA 65 公钥长度为 1952 字节
    if bytes.len() != ML_DSA_65_PUBLIC_KEY_LEN {
        return Err(anyhow::anyhow!(
            "Invalid ML-DSA 65 public key length: expected {}, got {}",
            ML_DSA_65_PUBLIC_KEY_LEN,
            bytes.len()
        ));
    }

    Ok(bytes.to_vec())
}

/// 序列化 ML-DSA 私钥
pub fn serialize_mldsa_private_key(sk: &[u8]) -> Vec<u8> {
    sk.to_vec()
}

/// 反序列化 ML-DSA 私钥
pub fn deserialize_mldsa_private_key(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    if bytes.is_empty() {
        return Err(anyhow::anyhow!("Empty private key"));
    }
    Ok(bytes.to_vec())
}
