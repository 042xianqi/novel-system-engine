//! 探针打包导出（world-gov）—— 把散落的探针 JSON 打成一个 `.wg-probe.tar.gz`。
//!
//! # 定位
//! 归档 / 备份 / 跨机转移复盘；**只含 world-gov 生成的结构化探针**，**绝不含资料库原始源文件**。
//!
//! 包内：`manifest.json`（版本/时间/layer过滤/增量锚点/基线指纹/条目列表）+ 各 `probe-xxx.json`。
//!
//! # 实现说明（零依赖）
//! 仓库无 tar/gzip 依赖 → 本模块**手写 USTAR 归档 + gzip(stored 块)**，产出**标准合法 `.tar.gz`**
//! （外部 `tar xzf` / Python `tarfile` 可直接解）。不使用压缩（stored），换取零依赖与零风险。
//!
//! # 铁则
//! - ❌ 不打包资料库源文件；❌ 不新增表；✅ 路径白名单（严格限定在允许目录内）。

use anyhow::Result;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// 包版本。
pub const WG_PROBE_VERSION: &str = "wg-probe/1";
/// 默认解包目录（相对库根）。
pub const UNPACK_SUBDIR: &str = "07_勘误工坊/probe_unpack";

// ────────────────────────────────── CRC32（gzip 需要）

/// CRC32（IEEE 802.3）—— gzip trailer 必需。
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

// ────────────────────────────────── gzip（stored 块，无压缩）

/// 把 `data` 包成 gzip（deflate stored 块，无压缩）。
pub fn gzip_stored(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 64);
    // --- gzip header (10 bytes) ---
    out.extend_from_slice(&[0x1f, 0x8b, 0x08, 0x00]); // magic, CM=8(deflate), FLG=0
    out.extend_from_slice(&[0, 0, 0, 0]); // MTIME = 0
    out.push(0x00); // XFL
    out.push(0xff); // OS = unknown
    // --- deflate: sequence of stored blocks ---
    let mut i = 0usize;
    if data.is_empty() {
        // 一个空 stored 块：BFINAL=1
        out.push(0x01);
        out.extend_from_slice(&[0x00, 0x00]); // LEN=0
        out.extend_from_slice(&[0xff, 0xff]); // NLEN
    }
    while i < data.len() {
        let chunk = (data.len() - i).min(0xffff);
        let is_last = i + chunk >= data.len();
        out.push(if is_last { 0x01 } else { 0x00 }); // BFINAL + BTYPE=00(stored)
        let len = chunk as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(&data[i..i + chunk]);
        i += chunk;
    }
    // --- trailer: CRC32 + ISIZE ---
    out.extend_from_slice(&crc32(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

// ────────────────────────────────── USTAR（tar）

/// 校验：归档内条目名必须安全（不含绝对路径/`..`/反斜杠/前导斜杠）。
pub fn is_safe_arcname(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && !name.contains("..")
        && !name.contains('\\')
        && !name.split('/').any(|seg| seg.is_empty() || seg == "." || seg == "..")
}

fn octal_field(buf: &mut [u8], value: u64) {
    // 形如 "0000000\0"：宽度-1 个八进制数字 + NUL
    let w = buf.len();
    let s = format!("{:0>width$o}", value, width = w - 1);
    let s = if s.len() > w - 1 { &s[s.len() - (w - 1)..] } else { &s };
    buf[..s.len()].copy_from_slice(s.as_bytes());
    buf[s.len()] = 0;
}

/// 构造一个 USTAR 头（512 字节）。
fn ustar_header(name: &str, size: u64, mtime: u64) -> Result<[u8; 512]> {
    if name.len() > 100 {
        anyhow::bail!("归档条目名过长（>100）：{name}");
    }
    let mut h = [0u8; 512];
    h[0..name.len()].copy_from_slice(name.as_bytes());
    octal_field(&mut h[100..108], 0o644); // mode
    octal_field(&mut h[108..116], 0); // uid
    octal_field(&mut h[116..124], 0); // gid
    octal_field(&mut h[124..136], size); // size
    octal_field(&mut h[136..148], mtime); // mtime
    // chksum：先填空格
    for b in &mut h[148..156] {
        *b = b' ';
    }
    h[156] = b'0'; // typeflag = regular file
    // magic + version
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    // uname/gname
    h[265..270].copy_from_slice(b"wgov\0");
    h[297..302].copy_from_slice(b"wgov\0");
    // checksum
    let sum: u64 = h.iter().map(|&b| b as u64).sum();
    let chk = format!("{:06o}\0 ", sum);
    h[148..156].copy_from_slice(chk.as_bytes());
    Ok(h)
}

/// 把 (arcname, data) 列表打成 tar（未压缩）。
pub fn tar_pack(entries: &[(String, Vec<u8>)], mtime: u64) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for (name, data) in entries {
        if !is_safe_arcname(name) {
            anyhow::bail!("归档条目名不安全（拒绝写入）：{name}");
        }
        let hdr = ustar_header(name, data.len() as u64, mtime)?;
        out.extend_from_slice(&hdr);
        out.extend_from_slice(data);
        // 补齐到 512 边界
        let pad = (512 - (data.len() % 512)) % 512;
        out.extend(std::iter::repeat(0u8).take(pad));
    }
    // 两个 512 零块结束
    out.extend(std::iter::repeat(0u8).take(1024));
    Ok(out)
}

/// 解 tar（支持 ustar 常规文件；返回 (name, data)）。**拒绝不安全条目名**。
pub fn tar_unpack(tar: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    let mut out = Vec::new();
    let mut off = 0usize;
    while off + 512 <= tar.len() {
        let hdr = &tar[off..off + 512];
        // 全零块 → 结束
        if hdr.iter().all(|&b| b == 0) {
            break;
        }
        let name_end = hdr[0..100].iter().position(|&b| b == 0).unwrap_or(100);
        let name = String::from_utf8_lossy(&hdr[0..name_end]).to_string();
        let size_s_raw = String::from_utf8_lossy(&hdr[124..136]);
        let size_s = size_s_raw.trim_matches(char::from(0)).trim();
        let size = u64::from_str_radix(size_s, 8).unwrap_or(0) as usize;
        let typeflag = hdr[156];
        off += 512;
        if typeflag == b'0' || typeflag == 0 {
            if !is_safe_arcname(&name) {
                anyhow::bail!("归档含不安全条目名，拒绝解包：{name}");
            }
            if off + size > tar.len() {
                anyhow::bail!("归档损坏：数据超出长度");
            }
            out.push((name, tar[off..off + size].to_vec()));
        }
        // 跳过数据 + 补齐
        off += size;
        let pad = (512 - (size % 512)) % 512;
        off += pad;
    }
    Ok(out)
}

// ────────────────────────────────── 高层：打包 / 查看 / 解包

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// 打包为 `.tar.gz`：manifest + 各探针。
/// `items`：(arcname, json_value)。返回 (路径, 字节数)。
pub fn create_archive(
    out_path: &Path,
    manifest: &Value,
    items: &[(String, Value)],
) -> Result<(PathBuf, usize)> {
    if let Some(dir) = out_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    entries.push(("manifest.json".to_string(), serde_json::to_vec_pretty(manifest)?));
    for (name, v) in items {
        let arc = format!("probes/{name}");
        entries.push((arc, serde_json::to_vec_pretty(v)?));
    }
    let tar = tar_pack(&entries, now_secs() as u64)?;
    let gz = gzip_stored(&tar);
    std::fs::write(out_path, &gz)?;
    Ok((out_path.to_path_buf(), gz.len()))
}

/// 读 archive 的 manifest（**不解压全部**：只解第一个条目 manifest.json）。
pub fn read_manifest(archive: &Path) -> Result<Value> {
    let raw = std::fs::read(archive)?;
    let tar = gunzip(&raw)?;
    let entries = tar_unpack(&tar)?;
    let m = entries
        .iter()
        .find(|(n, _)| n == "manifest.json")
        .ok_or_else(|| anyhow::anyhow!("归档缺少 manifest.json"))?;
    Ok(serde_json::from_slice(&m.1)?)
}

/// 解包到 `to_dir`（只写安全条目名；拒绝逃逸）。
pub fn unpack_archive(archive: &Path, to_dir: &Path) -> Result<usize> {
    let raw = std::fs::read(archive)?;
    let tar = gunzip(&raw)?;
    let entries = tar_unpack(&tar)?;
    let mut n = 0usize;
    for (name, data) in entries {
        if !is_safe_arcname(&name) {
            anyhow::bail!("拒绝解包不安全条目：{name}");
        }
        let p = to_dir.join(&name);
        // 双保险：目标必须仍在 to_dir 之下
        if !p.starts_with(to_dir) {
            anyhow::bail!("解包路径逃逸：{name}");
        }
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&p, &data)?;
        n += 1;
    }
    Ok(n)
}

/// 解 gzip（仅支持 stored 块 + 少量动态块回退不做；本工具只产 stored）。
pub fn gunzip(data: &[u8]) -> Result<Vec<u8>> {
    if data.len() < 18 || data[0] != 0x1f || data[1] != 0x8b {
        anyhow::bail!("不是合法 gzip（魔数不符）");
    }
    if data[2] != 8 {
        anyhow::bail!("不支持的 gzip 压缩方法 {}", data[2]);
    }
    let flg = data[3];
    let mut i = 10usize;
    if flg & 0x04 != 0 {
        // FEXTRA
        if i + 2 > data.len() { anyhow::bail!("gzip FEXTRA 越界"); }
        let xlen = u16::from_le_bytes([data[i], data[i + 1]]) as usize;
        i += 2 + xlen;
    }
    if flg & 0x08 != 0 {
        // FNAME
        while i < data.len() && data[i] != 0 { i += 1; }
        i += 1;
    }
    if flg & 0x10 != 0 {
        // FCOMMENT
        while i < data.len() && data[i] != 0 { i += 1; }
        i += 1;
    }
    if flg & 0x02 != 0 { i += 2; } // FHCRC
    // deflate stored 块
    let end = data.len().saturating_sub(8); // 去掉 trailer
    let mut out = Vec::new();
    while i < end {
        let hdr = data[i];
        let bfinal = hdr & 0x01 != 0;
        let btype = (hdr >> 1) & 0x03;
        i += 1;
        match btype {
            0 => {
                if i + 4 > end { anyhow::bail!("stored 块头越界"); }
                let len = u16::from_le_bytes([data[i], data[i + 1]]) as usize;
                i += 4;
                if i + len > end { anyhow::bail!("stored 块数据越界"); }
                out.extend_from_slice(&data[i..i + len]);
                i += len;
            }
            _ => anyhow::bail!("本工具只产 stored 块；遇到 btype={btype} 无法解（请用标准 gzip 工具）"),
        }
        if bfinal {
            break;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn crc32_known_vector() {
        // "123456789" 的 CRC32 = 0xCBF43926
        assert_eq!(crc32(b"123456789"), 0xCBF43926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn gzip_roundtrip_lossless() {
        for payload in [b"".to_vec(), b"hello".to_vec(), vec![b'x'; 200_000]] {
            let gz = gzip_stored(&payload);
            assert_eq!(gunzip(&gz).unwrap(), payload, "gzip 往返必须无损");
        }
    }

    #[test]
    fn tar_roundtrip_and_padding() {
        let entries = vec![
            ("manifest.json".to_string(), br#"{"v":1}"#.to_vec()),
            ("probes/a.json".to_string(), vec![b'z'; 600]),
        ];
        let tar = tar_pack(&entries, 0).unwrap();
        assert_eq!(tar.len() % 512, 0, "tar 必须 512 对齐");
        let back = tar_unpack(&tar).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].0, "manifest.json");
        assert_eq!(back[1].1.len(), 600);
    }

    #[test]
    fn arcname_safety_rejects_escape() {
        assert!(is_safe_arcname("probes/a.json"));
        assert!(is_safe_arcname("manifest.json"));
        for bad in ["../etc/passwd", "/abs", "a/../b", "a\\b", "", "a//b", "./x"] {
            assert!(!is_safe_arcname(bad), "必须拒绝：{bad}");
        }
        // 打包时遇不安全名 → 报错
        assert!(tar_pack(&[("../evil".into(), vec![1])], 0).is_err());
    }

    #[test]
    fn create_read_unpack_roundtrip() {
        let dir = std::env::temp_dir().join(format!("wg_export_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let arch = dir.join("t.wg-probe.tar.gz");
        let manifest = json!({"version": WG_PROBE_VERSION, "layers": "L2,L3", "baseline_fingerprint":"fp"});
        let items = vec![
            ("probe-a.json".to_string(), json!({"key":"A"})),
            ("probe-b.json".to_string(), json!({"key":"B"})),
        ];
        let (_p, n) = create_archive(&arch, &manifest, &items).unwrap();
        assert!(n > 0);
        // 读 manifest（不解压全部）
        let m = read_manifest(&arch).unwrap();
        assert_eq!(m["version"], WG_PROBE_VERSION);
        assert_eq!(m["baseline_fingerprint"], "fp");
        // 解包
        let to = dir.join("unpack");
        let cnt = unpack_archive(&arch, &to).unwrap();
        assert_eq!(cnt, 3, "manifest + 2 probes");
        assert!(to.join("manifest.json").is_file());
        assert!(to.join("probes/probe-a.json").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gunzip_rejects_non_gzip() {
        assert!(gunzip(b"not gzip data at all").is_err());
        assert!(gunzip(&[0x1f, 0x8b, 0x09, 0x00]).is_err(), "非 CM=8 应报错");
    }
}