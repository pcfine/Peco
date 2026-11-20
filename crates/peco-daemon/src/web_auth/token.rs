//! setup token 与会话票据的随机 / 摘要原语。
//!
//! 两者都是 32 字节随机数的 hex：一个证明「你有权把本地账号绑到密码上」，另一个
//! 是浏览器手里的会话凭据。形状相同但用途不同，故都在这里生成，免得两处各写一份。

use std::fs;
use std::path::Path;

use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::error::DaemonError;

/// 一份令牌的 hex 长度（32 字节）。
pub const TICKET_HEX_LEN: usize = 64;

/// 读 `path`；缺失就生成一份并落盘（0600）。
pub fn load_or_create(path: &Path) -> Result<String, DaemonError> {
    match fs::read_to_string(path) {
        Ok(raw) => {
            let token = raw.trim().to_string();
            if !is_ticket(&token) {
                return Err(DaemonError::Config(format!(
                    "token file '{}' must hold {TICKET_HEX_LEN} hex characters",
                    path.display()
                )));
            }
            Ok(token)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let token = new_ticket();
            write_private(path, &token)?;
            Ok(token)
        }
        Err(e) => Err(DaemonError::Io(e)),
    }
}

/// 令牌就是密码：建出来就只有属主可读，别指望事后再补一次 `chmod`。
fn write_private(path: &Path, token: &str) -> Result<(), DaemonError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    use std::io::Write;
    file.write_all(token.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

/// 32 字节随机数的 hex。
pub fn new_ticket() -> String {
    let mut bytes = [0u8; TICKET_HEX_LEN / 2];
    rand::rng().fill_bytes(&mut bytes);
    hex(&bytes)
}

pub fn sha256_hex(input: &str) -> String {
    hex(&Sha256::digest(input.as_bytes()))
}

/// 64 位 hex。文件内容与请求头里的候选值都按这个形状校验。
pub fn is_ticket(raw: &str) -> bool {
    raw.len() == TICKET_HEX_LEN && raw.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// 恒定时间比较：`==` 在第一个不同的字节上就返回，够攻击者逐字节试出令牌。
///
/// 长度差异照旧立刻返回 —— 长度不是秘密，逐字节比较才是。
pub fn constant_time_eq(left: &str, right: &str) -> bool {
    let (left, right) = (left.as_bytes(), right.as_bytes());
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 生成的文件必须 0600 且再读一次拿到同一份令牌 —— 重启不该换掉运维手里的令牌。
    #[cfg(unix)]
    #[test]
    fn a_missing_token_file_is_created_owner_only_and_reused() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("token");

        let token = load_or_create(&path).unwrap();
        assert!(is_ticket(&token), "{token} 应是 64 位 hex");
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "令牌文件只能属主可读");
        assert_eq!(
            load_or_create(&path).unwrap(),
            token,
            "已存在的令牌必须复用"
        );
    }

    /// 坏内容当启动错误报出来，不猜也不换：换掉等于把用户手里的令牌作废。
    #[test]
    fn a_broken_token_file_is_a_config_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        fs::write(&path, "not-a-token\n").unwrap();

        let err = load_or_create(&path).unwrap_err();
        assert_eq!(err.code(), "config_error");
        assert!(err.to_string().contains("64"), "{err}");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "not-a-token\n",
            "不得改写"
        );
    }

    /// 变异方法：把 `acc | (x ^ y)` 换成提前 `return false` 的相等比较 ——
    /// 结论仍然正确，所以只有「同长不同字节」这条用例能钉住实现确实走完全程。
    #[test]
    fn constant_time_eq_only_accepts_identical_strings() {
        let token = new_ticket();
        assert!(constant_time_eq(&token, &token));
        assert!(!constant_time_eq(&token, &token.to_uppercase()));
        let mut other = token.clone();
        other.pop();
        other.push(if token.ends_with('0') { '1' } else { '0' });
        assert!(!constant_time_eq(&token, &other), "末位不同必须判否");
        assert!(!constant_time_eq(&token, "short"));
    }

    /// 摘要只认输入本身：同一输入稳定、不同输入不同。
    #[test]
    fn ticket_hashes_are_stable_sha256_hex() {
        assert_eq!(sha256_hex("abc"), sha256_hex("abc"));
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_ne!(sha256_hex("abc"), sha256_hex("abd"));
        assert_eq!(sha256_hex("abc").len(), TICKET_HEX_LEN);
    }

    #[test]
    fn tickets_are_random_and_well_formed() {
        let first = new_ticket();
        let second = new_ticket();
        assert_ne!(first, second, "两次生成的令牌不得相同");
        assert!(is_ticket(&first) && is_ticket(&second));
        assert!(!is_ticket(""), "空串不是令牌");
        assert!(
            !is_ticket(&"g".repeat(TICKET_HEX_LEN)),
            "非 hex 字符不是令牌"
        );
    }
}
