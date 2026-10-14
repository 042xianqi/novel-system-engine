//! 轻量摘要（world-gov）—— 零依赖 SHA-1 实现，用于**内容指纹**。
//!
//! # 定位
//! 第 11 批差异快照需要稳定的 40-hex 内容指纹（`snap_id` / `content_hash`）。
//! 仓库无 sha1/sha2 依赖，故手写 SHA-1（RFC 3174）。**仅用于内容指纹，非安全用途**
//! （SHA-1 已不抗碰撞，但不引入任何密钥/认证语义，仅作内容变更检测）。
//!
//! # 铁则
//! - ✅ 纯函数，无副作用，无外部依赖。
//! - ✅ 标准测试向量自校验（`""` / `"abc"` / 长串）。

/// SHA-1 摘要（40 位小写 hex）。纯函数。
pub fn sha1_hex(data: &[u8]) -> String {
    let mut h0: u32 = 0x6745_2301;
    let mut h1: u32 = 0xEFCD_AB89;
    let mut h2: u32 = 0x98BA_DCFE;
    let mut h3: u32 = 0x1032_5476;
    let mut h4: u32 = 0xC3D2_E1F0;

    let ml = (data.len() as u64).wrapping_mul(8);
    let mut msg: Vec<u8> = Vec::with_capacity(data.len() + 72);
    msg.extend_from_slice(data);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&ml.to_be_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[i * 4], chunk[i * 4 + 1], chunk[i * 4 + 2], chunk[i * 4 + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h0, h1, h2, h3, h4);
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h0 = h0.wrapping_add(a);
        h1 = h1.wrapping_add(b);
        h2 = h2.wrapping_add(c);
        h3 = h3.wrapping_add(d);
        h4 = h4.wrapping_add(e);
    }
    format!("{h0:08x}{h1:08x}{h2:08x}{h3:08x}{h4:08x}")
}

/// 计算文件内容哈希；文件不存在/不可读 → 返回固定哨兵 `"ABSENT"`（**不报错、不 panic**）。
pub fn file_content_hash(path: &std::path::Path) -> String {
    match std::fs::read(path) {
        Ok(bytes) => sha1_hex(&bytes),
        Err(_) => "ABSENT".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_standard_vectors() {
        // RFC 3174 / FIPS 180 标准向量
        assert_eq!(sha1_hex(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            sha1_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
        // 100 万个 'a'
        let million_a = vec![b'a'; 1_000_000];
        assert_eq!(
            sha1_hex(&million_a),
            "34aa973cd4c4daa4f61eeb2bdbad27316534016f"
        );
    }

    #[test]
    fn sha1_length_is_40_hex_and_sensitive() {
        let a = sha1_hex("残响之河".as_bytes());
        assert_eq!(a.len(), 40);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, sha1_hex("残响之河2".as_bytes()));
    }

    #[test]
    fn file_hash_absent_is_sentinel() {
        let p = std::env::temp_dir().join("wg_digest_no_such_file_zzz.md");
        assert_eq!(file_content_hash(&p), "ABSENT");
        let f = std::env::temp_dir().join(format!("wg_digest_{}.txt", std::process::id()));
        std::fs::write(&f, b"abc").unwrap();
        assert_eq!(file_content_hash(&f), "a9993e364706816aba3e25717850c26c9cd0d89d");
        let _ = std::fs::remove_file(&f);
    }
}