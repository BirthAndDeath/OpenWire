// ==================== 使用示例 ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_software_token() {
        let hsm = SoftwareTokenHsm::new().unwrap();
        let mut alice = RootOfTrust::with_hsm(hsm).await.unwrap();

        // 模拟对等方 Bob
        let bob_secret = ReusableSecret::random_from_rng(OsRng);
        let bob_public = X25519PublicKey::from(&bob_secret);

        // Alice 建立会话
        alice.establish_session(&bob_public).await.unwrap();

        // 加密测试
        let nonce = [0u8; 12];
        let msg = "Hello, WebAuthn!";
        let encrypted = alice.encrypt_message(&msg, &nonce).unwrap();

        // 注意：解密需要 Bob 的 RootOfTrust 实例，这里仅测试加密流程
        assert!(!encrypted.ciphertext.is_empty());
    }
}