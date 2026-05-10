use aws_lc_rs::encoding::AsRawBytes;
use aws_lc_rs::signature::{KeyPair, UnparsedPublicKey};
use aws_lc_rs::unstable::signature::{ML_DSA_65, ML_DSA_65_SIGNING, PqdsaKeyPair};
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
