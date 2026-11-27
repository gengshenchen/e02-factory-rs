//! 设置页权限校验（SHA-256 哈希，不落盘明文密码）。
//!
//! 账号体系：
//! - 常规管理员账号：`admin`（内置默认密码，解锁后可在设置页修改，哈希存 `config.toml`）；
//! - 备用恢复账号：独立固定凭据（仅以 SHA-256 摘要内嵌，不写配置、不在界面暴露），
//!   用于现场忘记 `admin` 密码时解锁并重置。

/// 常规管理员用户名。
pub const ADMIN_USER: &str = "admin";

/// 默认 `admin` 凭据摘要（`e02-factory-auth-v1:admin:<default>`）。
const DEFAULT_ADMIN_HASH: &str = "fc98f97a5f5a253aa906e71444799ca60786cfd126a28758357b2dd408732107";

/// 备用恢复凭据摘要（仅比对哈希，不存明文）。
const MASTER_CRED_HASH: &str = "8ebd37ceef7109fe24ec1ea3af5dba1aae265f110cfe30b4481b4ec9c24f85c8";

/// 解锁后的权限角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthRole {
    /// 常规管理员
    Admin,
    /// 备用恢复管理员（可重置 admin 密码）
    Master,
}

/// 凭据加域摘要（用户名不区分大小写且去首尾空白，密码严格区分大小写）。
pub fn hash_password(username: &str, password: &str) -> String {
    let payload = format!(
        "e02-factory-auth-v1:{}:{}",
        username.trim().to_ascii_lowercase(),
        password
    );
    sha256_hex(payload.as_bytes())
}

/// 校验账号密码，通过返回对应角色，否则返回 `None`。
pub fn verify(username: &str, password: &str, stored_admin_hash: &str) -> Option<AuthRole> {
    let user = username.trim();
    if user.is_empty() || password.is_empty() {
        return None;
    }
    let digest = hash_password(user, password);
    if ct_eq(&digest, MASTER_CRED_HASH) {
        return Some(AuthRole::Master);
    }
    if user.eq_ignore_ascii_case(ADMIN_USER) {
        let expected = if stored_admin_hash.trim().is_empty() {
            DEFAULT_ADMIN_HASH
        } else {
            stored_admin_hash.trim()
        };
        if ct_eq(&digest, expected) {
            return Some(AuthRole::Admin);
        }
    }
    None
}

/// 校验新密码格式并生成 `admin` 密码哈希。
pub fn make_admin_hash(new_password: &str, confirm: &str) -> Result<String, &'static str> {
    if new_password.len() < 4 {
        return Err("新密码至少需要 4 位字符");
    }
    if new_password.chars().any(char::is_control) {
        return Err("密码不能包含控制字符");
    }
    if new_password != confirm {
        return Err("两次输入的新密码不一致");
    }
    Ok(hash_password(ADMIN_USER, new_password))
}

/// 常量时间字节比较。
fn ct_eq(a: &str, b: &str) -> bool {
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    if ab.len() != bb.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in ab.iter().zip(bb.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// 纯 Rust FIPS 180-4 SHA-256（小写十六进制）。
pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut buf = Vec::with_capacity(data.len() + 72);
    buf.extend_from_slice(data);
    buf.push(0x80);
    while (buf.len() % 64) != 56 {
        buf.push(0);
    }
    buf.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in buf.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        for (dst, add) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *dst = dst.wrapping_add(add);
        }
    }

    let mut out = String::with_capacity(64);
    for word in h {
        out.push_str(&format!("{word:08x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_nist_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn default_admin_and_master_and_password_change() {
        // 默认 admin 密码通过，错密码拒绝
        assert_eq!(verify("admin", "admin", ""), Some(AuthRole::Admin));
        assert_eq!(verify(" ADMIN ", "admin", ""), Some(AuthRole::Admin));
        assert_eq!(verify("admin", "Admin", ""), None);
        assert_eq!(verify("admin", "admin123456", ""), None);
        assert_eq!(verify("admin", "wrong", ""), None);
        assert_eq!(verify("other", "admin", ""), None);

        // 修改 admin 密码后：新密码通过、旧默认密码失效
        let custom_hash = make_admin_hash("newpass888", "newpass888").unwrap();
        assert_eq!(
            verify("admin", "newpass888", &custom_hash),
            Some(AuthRole::Admin)
        );
        assert_eq!(verify("admin", "admin", &custom_hash), None);

        // 允许将密码改回 admin（5 位）
        let back_hash = make_admin_hash("admin", "admin").unwrap();
        assert_eq!(verify("admin", "admin", &back_hash), Some(AuthRole::Admin));

        // 备用万能账号无论 admin 密码是否修改均可用
        assert_eq!(verify("hecsion", "hecsion", ""), Some(AuthRole::Master));
        assert_eq!(
            verify("hecsion", "hecsion", &custom_hash),
            Some(AuthRole::Master)
        );
        assert_eq!(verify("hecsion", "hc123456", &custom_hash), None);
        assert_eq!(verify("hecsion", "wrong", &custom_hash), None);

        // 密码规则校验
        assert!(make_admin_hash("123", "123").is_err());
        assert!(make_admin_hash("123456", "654321").is_err());
    }
}
