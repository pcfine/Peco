//! 密码的入参校验与摘要。
//!
//! 摘要是「sha256 后再 bcrypt」：bcrypt 只看前 72 字节，直接喂 128 字符的口令会让
//! 尾部静默失效（两个只有后半段不同的口令可以互换）。摘要固定 64 位 hex、无 NUL，
//! 既落在 72 字节内，又把全长的差异带进摘要。

use sha2::{Digest, Sha256};

/// 入参下限：短口令在离线爆破面前等于没有密码。
pub const MIN_PASSWORD: usize = 8;
pub const MAX_PASSWORD: usize = 128;
pub const MAX_USERNAME: usize = 64;

/// bcrypt 成本：每次校验约几百毫秒，登录接口的限流按这个量级设计。
const BCRYPT_COST: u32 = 12;

/// 未知用户名也要跑一次校验用的固定摘要（成本与真实摘要一致）。
///
/// 改动 `BCRYPT_COST` 时这条要同步重建，`dummy_hash_matches_the_cost` 会判红。
const DUMMY_HASH: &str = "$2b$12$.EdGVONl6RlfaaqpRHc4GeU8XiB18xMUvSKmMFctrl4OZA.T2KGAO";

#[derive(Debug, thiserror::Error)]
pub enum PasswordError {
    #[error("bcrypt failed: {0}")]
    Bcrypt(#[from] bcrypt::BcryptError),
    #[error("password hashing task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
}

/// 用户名去空白后 1..=64；空白不算名字。
pub fn validate_username(raw: &str) -> Result<String, &'static str> {
    let name = raw.trim();
    if name.is_empty() {
        return Err("username is empty");
    }
    if name.chars().count() > MAX_USERNAME {
        return Err("username is longer than 64 characters");
    }
    Ok(name.to_string())
}

/// 密码按字符数计 8..=128，且不裁剪：首尾空格是密码的一部分。
pub fn validate_password(raw: &str) -> Result<(), &'static str> {
    let len = raw.chars().count();
    if len < MIN_PASSWORD {
        return Err("password must be at least 8 characters");
    }
    if len > MAX_PASSWORD {
        return Err("password must be at most 128 characters");
    }
    Ok(())
}

/// 摘要。bcrypt 是纯 CPU 工作（成本 12 约数百毫秒），放到阻塞线程池，别占 reactor。
pub async fn hash(password: &str) -> Result<String, PasswordError> {
    let password = digest_input(password);
    Ok(tokio::task::spawn_blocking(move || bcrypt::hash(password, BCRYPT_COST)).await??)
}

/// 校验。摘要损坏按「不通过」处理并留日志：认证判定只允许失败成拒绝。
pub async fn verify(password: &str, hash: &str) -> bool {
    let password = digest_input(password);
    let hash = hash.to_string();
    match tokio::task::spawn_blocking(move || bcrypt::verify(password, &hash)).await {
        Ok(Ok(passed)) => passed,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "stored password hash is unusable");
            false
        }
        Err(e) => {
            tracing::error!(error = %e, "password verification task failed");
            false
        }
    }
}

/// 烧掉一次与真实校验等量的时间；结论一定是否。
pub async fn verify_against_dummy(password: &str) {
    let _ = verify(password, DUMMY_HASH).await;
}

/// 抹平用户名的存在性：入参长度差异不该体现在耗时上，故摘要总是 64 位。
fn digest_input(password: &str) -> String {
    let digest = Sha256::digest(password.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 摘要第二段是成本；解析失败即夹具坏了，直接失败比比较一个假成本有用。
    fn cost_of(hash: &str) -> u32 {
        hash.split('$')
            .nth(2)
            .and_then(|cost| cost.parse().ok())
            .unwrap_or_else(|| panic!("{hash} 不是 bcrypt 摘要"))
    }

    #[test]
    fn usernames_are_trimmed_and_bounded() {
        assert_eq!(validate_username("  peco  ").unwrap(), "peco");
        assert!(validate_username("   ").is_err());
        assert!(validate_username("").is_err());
        assert!(validate_username(&"a".repeat(MAX_USERNAME)).is_ok());
        assert!(validate_username(&"a".repeat(MAX_USERNAME + 1)).is_err());
    }

    /// 变异方法：把字符数换成字节数 —— 中文口令的边界断言变红。
    #[test]
    fn passwords_are_counted_in_characters() {
        assert!(validate_password("1234567").is_err());
        assert!(validate_password("12345678").is_ok());
        // 8 个中文字符 = 24 字节：按字节算会误判为合法。
        assert!(validate_password(&"密".repeat(MIN_PASSWORD)).is_ok());
        assert!(validate_password(&"a".repeat(MAX_PASSWORD)).is_ok());
        assert!(validate_password(&"a".repeat(MAX_PASSWORD + 1)).is_err());
        assert!(
            validate_password(" pass word ").is_ok(),
            "空格是密码的一部分"
        );
    }

    /// 摘要里不含明文，且同一口令每次摘要不同（bcrypt 自带随机盐）。
    #[tokio::test]
    async fn hashes_are_salted_and_verifiable() {
        let first = hash("correct horse").await.unwrap();
        let second = hash("correct horse").await.unwrap();
        assert_ne!(first, second, "随机盐让两次摘要不同");
        assert!(!first.contains("correct horse"));
        assert_eq!(cost_of(&first), BCRYPT_COST);
        assert!(verify("correct horse", &first).await);
        assert!(!verify("wrong horse", &first).await);
    }

    /// 变异方法：去掉 `digest_input` 的前置摘要 ⇒ 「尾部不同仍通过校验」变红。
    #[tokio::test]
    async fn characters_past_the_bcrypt_limit_still_matter() {
        let head = "a".repeat(72);
        let hash = super::hash(&format!("{head}tail-one")).await.unwrap();
        assert!(verify(&format!("{head}tail-one"), &hash).await);
        assert!(
            !verify(&format!("{head}tail-two"), &hash).await,
            "第 73 字节之后的差异被吞掉了"
        );
    }

    /// 损坏的摘要只判否，不 panic、不 500。
    #[tokio::test]
    async fn a_broken_stored_hash_is_a_denial() {
        assert!(!verify("whatever", "not-a-hash").await);
        assert!(!verify("whatever", "").await);
    }

    #[test]
    fn dummy_hash_matches_the_cost() {
        assert_eq!(
            cost_of(DUMMY_HASH),
            BCRYPT_COST,
            "dummy 摘要的成本必须与真实摘要一致，否则耗时差照样泄漏用户名的存在性"
        );
    }
}
