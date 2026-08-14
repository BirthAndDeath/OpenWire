use aws_lc_rs::encoding::AsRawBytes;
use aws_lc_rs::signature::{KeyPair, UnparsedPublicKey};
use aws_lc_rs::signature::{ML_DSA_65, ML_DSA_65_SIGNING, PqdsaKeyPair};
/// ML-DSA 密钥对类型别名 (public_key, secret_key)
pub type MlDsaKeypair = (Vec<u8>, Vec<u8>);

/// ML-DSA 65 公钥长度（字节）
pub const ML_DSA_65_PUBLIC_KEY_LEN: usize = 1952;
/// ML-DSA 65 私钥长度（字节）
pub const ML_DSA_65_PRIVATE_KEY_LEN: usize = 4032;
/// ML-DSA 65 签名长度（字节）
pub const ML_DSA_65_SIGNATURE_LEN: usize = 3309;

/// 验证 ML-DSA 公钥字节是否为有效密钥（密码学验证）
///
/// 使用 aws-lc-rs 的 `UnparsedPublicKey::parse()` 方法对公钥字节进行
/// 密码学级别的格式验证（包括算法匹配、编码有效性和系数范围检查）。
///
/// # 参数
/// - pubkey: ML-DSA 公钥字节（原始格式，非 hex）
///
/// # 返回
/// - true 如果公钥有效，false 则无效
pub fn validate_mldsa_pubkey_bytes(pubkey: &[u8]) -> bool {
    if pubkey.len() != ML_DSA_65_PUBLIC_KEY_LEN {
        return false;
    }
    UnparsedPublicKey::new(&ML_DSA_65, pubkey)
        .parse()
        .is_ok()
}

/// 验证 ML-DSA 公钥 hex 字符串是否为有效密钥（格式 + 密码学验证）
///
/// 先验证 hex 编码格式和长度，再通过 aws-lc-rs 进行密码学级别验证
/// （算法匹配、编码有效性、多项式系数范围检查）。
///
/// # 参数
/// - hex: ML-DSA 公钥的十六进制字符串（3904 字符）
///
/// # 返回
/// - true 如果公钥有效，false 则无效
pub fn validate_mldsa_pubkey_hex(hex: &str) -> bool {
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return false;
    }
    match hex::decode(hex) {
        Ok(bytes) => validate_mldsa_pubkey_bytes(&bytes),
        Err(_) => false,
    }
}

/// 生成新的 ML-DSA 密钥对用于持久化身份
///
/// # 返回
/// - public_key: 公钥(用于验证签名)
/// - secret_key: 私钥(原始格式)
pub fn generate_mldsa_keypair() -> crate::error::SignatureResult<MlDsaKeypair> {
    let key_pair = PqdsaKeyPair::generate(&ML_DSA_65_SIGNING)
        .map_err(crate::error::SignatureError::GenerateMlDsaKeypairFailed)?;

    let public_key = key_pair.public_key().as_ref().to_vec();
    let secret_key = key_pair
        .private_key()
        .as_raw_bytes()
        .map_err(|_| crate::error::SignatureError::EmptyPrivateKey)?
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
pub fn sign_data(private_key: &[u8], data: &[u8]) -> crate::error::SignatureResult<Vec<u8>> {
    let key_pair = PqdsaKeyPair::from_raw_private_key(&ML_DSA_65_SIGNING, private_key)
        .map_err(crate::error::SignatureError::ParseMlDsaPrivateKeyFailed)?;

    let mut signature = vec![0u8; ML_DSA_65_SIGNATURE_LEN];
    key_pair
        .sign(data, &mut signature)
        .map_err(crate::error::SignatureError::SignDataFailed)?;

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
pub fn verify_signature(
    public_key: &[u8],
    data: &[u8],
    signature: &[u8],
) -> crate::error::SignatureResult<bool> {
    let public_key_obj = UnparsedPublicKey::new(&ML_DSA_65, public_key);

    match public_key_obj.verify(data, signature) {
        Ok(_) => Ok(true),
        Err(_) => Ok(false),
    }
}
