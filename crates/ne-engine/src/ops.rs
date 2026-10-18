//! 运维层 —— `cold_backup.py` + `pipeline_backend.py` 的 Rust 移植。
//!
//! `cold_backup`：02_资料库 逐卡 sha256[0:12] 清单 + 确定性 tar.gz 打包。
//! `pipeline_backend`：dynamics 流水日志 / 伏笔回收 / 漏斗推送（DB 侧）。
//!
//! **修复的历史 bug**：`cold_backup.py:34` 曾经也用硬编码 ROOT（后改为
//! `Path(__file__).parent.parent.parent`，但该推导在 db/ 被移出 05_管线框架 后即失效）。
//! Rust 版全部路径由调用方注入。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

// ═══════════════════════════ cold_backup.py

/// 19 个资料库分类（照抄 `CATS`）。
pub const CATS: [&str; 19] = [
    "世界观", "丹药", "事件", "体系力量", "关系", "力量体系", "功法", "势力", "因果线", "地图",
    "天材地宝", "文化", "文明", "时间", "科技", "装备", "角色", "诡异", "跨文明关系",
];

/// 排除的子目录（照抄 `EXCLUDE_DIRS`）。
pub const EXCLUDE_DIRS: [&str; 4] = ["_bak", "_backup", "_索引", "_规范"];

/// 确定性 tar 参数（照抄 `TAR_DET_ARGS`，总方案 §四 锁死口径）。
pub const TAR_DET_ARGS: [&str; 6] = [
    "--sort=name",
    "--owner=0",
    "--group=0",
    "--numeric-owner",
    "--mtime=@0",
    "--format=ustar",
];

/// 文件 sha256（十六进制全量）。
pub fn sha256_file(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).with_context(|| format!("打开失败 {path:?}"))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finish_hex())
}

/// 短 sha（前 12 位，照抄 `_short_sha12`）。
pub fn short_sha12(path: &Path) -> Result<String> {
    Ok(sha256_file(path)?[..12].to_string())
}

/// 遍历某分类下的`.md`卡（照抄 `iter_cards`）：排除 `EXCLUDE_DIRS`，排序输出。
pub fn iter_cards(matlib: &Path, cat: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let dir = matlib.join(cat);
    if !dir.is_dir() {
        return out;
    }
    walk_md(&dir, &mut out);
    out.sort();
    out
}

fn walk_md(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if p.is_dir() {
            if EXCLUDE_DIRS.contains(&name.as_str()) {
                continue;
            }
            walk_md(&p, out);
        } else if name.ends_with(".md") {
            out.push(p);
        }
    }
}

/// 逐卡清单（照抄 `build_manifest`）：key = `02_资料库/<相对路径>`，value = sha12。
pub fn build_manifest(matlib: &Path) -> Result<(BTreeMap<String, String>, usize)> {
    let mut manifest = BTreeMap::new();
    let mut total = 0usize;
    for cat in CATS {
        for p in iter_cards(matlib, cat) {
            let rel = p
                .strip_prefix(matlib)
                .with_context(|| format!("{p:?} 不在 {matlib:?} 下"))?;
            let key = format!("02_资料库/{}", rel.to_string_lossy().replace('\\', "/"));
            manifest.insert(key, short_sha12(&p)?);
            total += 1;
        }
    }
    Ok((manifest, total))
}

/// 清单写 jsonl 文本（照抄 `write_jsonl`：`path<TAB>sha12`，按路径排序）。
///
/// 返回字符串而非直接落盘 —— IO 由调用方决定（便于单测）。
pub fn manifest_to_jsonl(manifest: &BTreeMap<String, String>) -> String {
    let mut s: String = manifest.iter().map(|(k, v)| format!("{k}\t{v}\n")).collect();
    if s.is_empty() {
        s.push('\n');
    }
    s
}

/// 冷备份快照产物描述（对应 `cmd_snapshot` 的产出，不含 IO）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotPlan {
    /// tar 文件名（`{date}_v1/cards.tar.gz`）
    pub tar_rel: String,
    /// 清单文件名
    pub manifest_rel: String,
    /// 待打包文件（相对 matlib，已排序）
    pub files: Vec<String>,
    pub card_count: usize,
}

/// 规划一次快照（照抄 `cmd_snapshot` 的文件收集 + 命名，去 IO）。
pub fn plan_snapshot(matlib: &Path, date: &str) -> Result<SnapshotPlan> {
    let (manifest, count) = build_manifest(matlib)?;
    let dir = format!("{date}_v1");
    Ok(SnapshotPlan {
        tar_rel: format!("{dir}/cards.tar.gz"),
        manifest_rel: format!("{dir}/manifest.jsonl"),
        files: manifest.keys().cloned().collect(),
        card_count: count,
    })
}

/// tar 命令行（照抄 `tar_deterministic` 的参数组装）。
pub fn tar_argv(out_tar_gz: &Path, files: &[String]) -> Vec<String> {
    let mut v: Vec<String> = vec!["tar".into(), "-czf".into(), out_tar_gz.to_string_lossy().into()];
    v.extend(TAR_DET_ARGS.iter().map(|s| s.to_string()));
    v.push("--files-from=-".into());
    v.extend(files.iter().cloned());
    v
}

// ═══════════════════════════ pipeline_backend.py

/// 小说 ID（照抄 `NOVEL_ID`）。
pub const NOVEL_ID: &str = "canxiang";

/// dynamics 日志载荷（对应 `log_dynamics` 的 payload）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicsLog {
    pub novel_id: String,
    pub volume: String,
    pub chapter: u32,
    pub domain: String,
    pub seq: i64,
    pub note: String,
}

/// 构造 dynamics 日志（照抄 `log_dynamics` 的字段组装，去 DB）。
pub fn build_dynamics_log(
    volume: &str,
    chapter: u32,
    domain: &str,
    seq: i64,
    note: &str,
) -> DynamicsLog {
    DynamicsLog {
        novel_id: NOVEL_ID.to_string(),
        volume: volume.to_string(),
        chapter,
        domain: domain.to_string(),
        seq,
        note: note.to_string(),
    }
}

/// 伏笔回收记录（对应 `resolve_foreshadow`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForeshadowResolve {
    pub fs_id: String,
    pub chapter: u32,
    pub status: String,
    pub resolved_at: String,
}

/// 构造伏笔回收（照抄 `resolve_foreshadow`：status='resolved'）。
pub fn build_foreshadow_resolve(fs_id: &str, chapter: u32, resolved_at: &str) -> ForeshadowResolve {
    ForeshadowResolve {
        fs_id: fs_id.to_string(),
        chapter,
        status: "resolved".to_string(),
        resolved_at: resolved_at.to_string(),
    }
}

/// 漏斗推送包（对应 `push_funnel`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunnelPush {
    pub novel_id: String,
    pub volume: String,
    pub chapter: u32,
}

/// 构造漏斗推送（照抄 `push_funnel`）。
pub fn build_funnel_push(volume: &str, chapter: u32) -> FunnelPush {
    FunnelPush {
        novel_id: NOVEL_ID.to_string(),
        volume: volume.to_string(),
        chapter,
    }
}

/// 索引校验结论（对应 `verify_index`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexCheck {
    pub ok: bool,
    pub missing: Vec<String>,
    pub detail: String,
}

/// 校验索引完整性（照抄 `verify_index`：清单键 vs 实盘键）。
pub fn verify_index(manifest_keys: &[String], actual_keys: &[String]) -> IndexCheck {
    let m: std::collections::BTreeSet<&String> = manifest_keys.iter().collect();
    let a: std::collections::BTreeSet<&String> = actual_keys.iter().collect();
    let missing: Vec<String> = m.difference(&a).map(|s| (*s).clone()).collect();
    let extra: Vec<String> = a.difference(&m).map(|s| (*s).clone()).collect();
    IndexCheck {
        ok: missing.is_empty() && extra.is_empty(),
        detail: format!("清单={} 实盘={} 缺={} 多={}", m.len(), a.len(), missing.len(), extra.len()),
        missing,
    }
}

// ───────────────────────── 纯 Rust SHA-256（无需外部 crate）

/// 最小 SHA-256 实现（FIPS 180-4），保证与 Python `hashlib.sha256` 逐位一致。
pub struct Sha256 {
    state: [u32; 8],
    buf: Vec<u8>,
    len: u64,
}

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    pub fn new() -> Self {
        Sha256 {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            buf: Vec::new(),
            len: 0,
        }
    }

    pub fn update(&mut self, data: &[u8]) {
        self.len += data.len() as u64;
        self.buf.extend_from_slice(data);
        while self.buf.len() >= 64 {
            let block: Vec<u8> = self.buf.drain(..64).collect();
            self.compress(&block);
        }
    }

    fn compress(&mut self, block: &[u8]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([block[i * 4], block[i * 4 + 1], block[i * 4 + 2], block[i * 4 + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut h = self.state;
        for i in 0..64 {
            let s1 = h[4].rotate_right(6) ^ h[4].rotate_right(11) ^ h[4].rotate_right(25);
            let ch = (h[4] & h[5]) ^ ((!h[4]) & h[6]);
            let t1 = h[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = h[0].rotate_right(2) ^ h[0].rotate_right(13) ^ h[0].rotate_right(22);
            let maj = (h[0] & h[1]) ^ (h[0] & h[2]) ^ (h[1] & h[2]);
            let t2 = s0.wrapping_add(maj);
            h[7] = h[6];
            h[6] = h[5];
            h[5] = h[4];
            h[4] = h[3].wrapping_add(t1);
            h[3] = h[2];
            h[2] = h[1];
            h[1] = h[0];
            h[0] = t1.wrapping_add(t2);
        }
        for i in 0..8 {
            self.state[i] = self.state[i].wrapping_add(h[i]);
        }
    }

    pub fn finish_hex(mut self) -> String {
        let bitlen = self.len * 8;
        self.buf.push(0x80);
        while self.buf.len() % 64 != 56 {
            self.buf.push(0);
        }
        self.buf.extend_from_slice(&bitlen.to_be_bytes());
        let blocks: Vec<u8> = std::mem::take(&mut self.buf);
        for chunk in blocks.chunks(64) {
            self.compress(chunk);
        }
        self.state.iter().map(|x| format!("{x:08x}")).collect()
    }

    /// 便捷：一次性算字节串摘要。
    pub fn digest_hex(data: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(data);
        h.finish_hex()
    }
}

/// 便捷：算字符串 sha256。
pub fn sha256_hex_str(s: &str) -> String {
    Sha256::digest_hex(s.as_bytes())
}

/// 便捷：算 JSON 值的稳定 sha256（键排序序列化）。
pub fn sha256_json(v: &Value) -> String {
    let s = serde_json::to_string(v).unwrap_or_default();
    sha256_hex_str(&s)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 铁证：自实现 SHA-256 必须与 Python hashlib 逐位一致（已知向量）。
    #[test]
    fn sha256_matches_known_vectors() {
        assert_eq!(
            sha256_hex_str(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex_str("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex_str("abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        // 跨块边界（>64 字节）
        let long: String = "a".repeat(1000);
        assert_eq!(
            sha256_hex_str(&long),
            "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
        );
    }

    #[test]
    fn short_sha12_is_first_12() {
        assert_eq!(&sha256_hex_str("abc")[..12], "ba7816bf8f01");
    }

    #[test]
    fn manifest_and_jsonl_sorted_and_tab_separated() {
        let mut m = BTreeMap::new();
        m.insert("02_资料库/角色/a.md".to_string(), "111111111111".to_string());
        m.insert("02_资料库/角色/b.md".to_string(), "222222222222".to_string());
        let s = manifest_to_jsonl(&m);
        assert_eq!(
            s,
            "02_资料库/角色/a.md\t111111111111\n02_资料库/角色/b.md\t222222222222\n"
        );
    }

    #[test]
    fn iter_cards_excludes_bak_dirs() {
        let tmp = std::env::temp_dir().join(format!("ops_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let role = tmp.join("角色");
        std::fs::create_dir_all(role.join("_bak")).unwrap();
        std::fs::write(role.join("a.md"), "x").unwrap();
        std::fs::write(role.join("_bak").join("old.md"), "x").unwrap();
        let cards = iter_cards(&tmp, "角色");
        assert_eq!(cards.len(), 1, "{cards:?}");
        assert!(cards[0].to_string_lossy().ends_with("a.md"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn build_manifest_keys_are_prefixed() {
        let tmp = std::env::temp_dir().join(format!("ops_mf_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("角色")).unwrap();
        std::fs::write(tmp.join("角色/a.md"), "abc").unwrap();
        let (m, n) = build_manifest(&tmp).unwrap();
        assert_eq!(n, 1);
        assert!(m.contains_key("02_资料库/角色/a.md"), "{:?}", m.keys());
        assert_eq!(m["02_资料库/角色/a.md"], "ba7816bf8f01");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn tar_argv_has_deterministic_flags() {
        let argv = tar_argv(Path::new("/out/cards.tar.gz"), &["a.md".to_string()]);
        assert_eq!(argv[0], "tar");
        for f in TAR_DET_ARGS {
            assert!(argv.contains(&f.to_string()), "缺确定性参数 {f}");
        }
        assert_eq!(argv.last().unwrap(), "a.md");
    }

    #[test]
    fn plan_snapshot_names() {
        let tmp = std::env::temp_dir().join(format!("ops_plan_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let p = plan_snapshot(&tmp, "2026-10-10").unwrap();
        assert_eq!(p.tar_rel, "2026-10-10_v1/cards.tar.gz");
        assert_eq!(p.manifest_rel, "2026-10-10_v1/manifest.jsonl");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn dynamics_log_shape() {
        let l = build_dynamics_log("卷01", 3, "01_力量", 5, "note");
        assert_eq!(l.novel_id, "canxiang");
        assert_eq!(l.seq, 5);
    }

    #[test]
    fn foreshadow_resolve_sets_status() {
        let r = build_foreshadow_resolve("F-001", 3, "2026-10-10");
        assert_eq!(r.status, "resolved");
    }

    #[test]
    fn verify_index_detects_missing() {
        let m = vec!["a".to_string(), "b".to_string()];
        let a = vec!["a".to_string()];
        let c = verify_index(&m, &a);
        assert!(!c.ok);
        assert_eq!(c.missing, vec!["b"]);
        assert!(verify_index(&m, &m).ok);
    }
}

// ═══════════════════════════ fill.py

/// 对象编号正则（照抄 `OBJ_RE`）：`XX-11-22` 形态（可被反引号包裹）。
pub fn obj_re() -> &'static regex::Regex {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"`?([A-Z]{2,5}(?:-\d+){1,3})`?").unwrap())
}

/// 细纲单章（对应 `_outline_chapters` 返回的 dict）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutlineChapter {
    pub no: u32,
    pub title: String,
    pub text: String,
}

/// 解析细纲为章列表（照抄 `_outline_chapters`）。
///
/// ⚠ Python 用 `(?s)###\s*第(\d+)章\s*(.+?)(?=\n###\s*第|\Z)` 含 **look-ahead**，
/// Rust `regex` crate 不支持；改用「按章头切分 + 取章头行余下部分」的等价实现。
pub fn outline_chapters(text: &str) -> Vec<OutlineChapter> {
    let head = regex::Regex::new(r"(?m)^###\s*第(\d+)章[ \t]*(.*)$").unwrap();
    let caps: Vec<(usize, u32, String)> = head
        .captures_iter(text)
        .map(|c| {
            let no: u32 = c.get(1).unwrap().as_str().parse().unwrap_or(0);
            let title = c.get(2).map(|m| m.as_str().trim().to_string()).unwrap_or_default();
            (c.get(0).unwrap().start(), no, title)
        })
        .collect();

    let mut out = Vec::new();
    for (i, (start, no, title)) in caps.iter().enumerate() {
        let end = caps.get(i + 1).map(|(s, _, _)| *s).unwrap_or(text.len());
        out.push(OutlineChapter {
            no: *no,
            // Python 取 `(.+?)` 首个非空行 → 即章头行的标题部分
            title: title.clone(),
            text: text[*start..end].to_string(),
        });
    }
    out
}

/// 跨章编号统计结果（对应 `fill_foreshadow` 的 first/last/cnt）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForeshadowStat {
    pub code: String,
    pub first_chapter: u32,
    pub last_chapter: u32,
    pub count: usize,
    pub first_title: String,
    pub content: String,
}

/// 从细纲提取跨章复现的编号 → 伏笔（照抄 `fill_foreshadow` 的统计口径）。
///
/// 只保留「出现 ≥2 次且末次章 > 首现章」的编号（埋设→兑现）。
pub fn extract_foreshadows(chapters: &[OutlineChapter]) -> Vec<ForeshadowStat> {
    use std::collections::BTreeMap;
    let re = obj_re();
    let mut first: BTreeMap<String, u32> = BTreeMap::new();
    let mut last: BTreeMap<String, u32> = BTreeMap::new();
    let mut cnt: BTreeMap<String, usize> = BTreeMap::new();

    for c in chapters {
        for cap in re.captures_iter(&c.text) {
            let code = cap.get(1).unwrap().as_str();
            if code.chars().all(|ch| ch.is_ascii_digit()) {
                continue;
            }
            first.entry(code.to_string()).or_insert(c.no);
            last.insert(code.to_string(), c.no);
            *cnt.entry(code.to_string()).or_insert(0) += 1;
        }
    }

    let mut out = Vec::new();
    for (code, fch) in &first {
        let lch = last[code];
        if cnt[code] < 2 || lch <= *fch {
            continue;
        }
        let title = chapters
            .iter()
            .find(|c| c.no == *fch)
            .map(|c| c.title.clone())
            .unwrap_or_default();
        out.push(ForeshadowStat {
            code: code.clone(),
            first_chapter: *fch,
            last_chapter: lch,
            count: cnt[code],
            first_title: title.clone(),
            content: format!("{code}（首现第{fch}章 {title}，第{lch}章须兑现）"),
        });
    }
    out
}

/// 主角种子状态（照抄 `fill_char_state` 的 seed）。
pub fn protagonist_seed() -> (String, Value) {
    (
        "CHAR-99-001".to_string(),
        serde_json::json!({
            "name": "陈寂",
            "realm": "炼气期（推演阶U0·穿越者金手指）",
            "location": "太玄帝国·青牛镇·陈家村",
            "status": "存活",
            "notes": "细纲卷01锚点：陈寂=穿越者·脑子里多了块推演面板·前世背景一概不介绍",
        }),
    )
}

/// 世界时间线索（照抄 `fill_timeline` 的正则）。
pub fn world_time_of(chapter_text: &str) -> String {
    let re = regex::Regex::new(r"(汐潮历[^，。\n]*|第[\d一二三四五六七八九十]+纪元[^，。\n]*)").unwrap();
    re.captures(chapter_text)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().trim().to_string())
        .unwrap_or_default()
}

/// 事件登记行（对应 `fill_events` 的 insert 值）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRow {
    pub id: String,
    pub name: String,
    pub status: String,
    pub notes: String,
}

/// 从 graph_事件.json 抽主线事件（照抄 `fill_events` 第一段）。
pub fn events_from_graph(graph: &Value, h1_of: impl Fn(&str) -> Option<String>) -> Vec<EventRow> {
    let mut out = Vec::new();
    for eid in graph
        .get("event_cards_mainline")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
    {
        let Some(eid) = eid.as_str() else { continue };
        let h1 = h1_of(eid).unwrap_or_default();
        let name = {
            let t = regex::Regex::new(r"^#\s*").unwrap().replace(&h1, "").to_string();
            let v = t.split('｜').last().unwrap_or("").trim().to_string();
            if v.is_empty() { eid.to_string() } else { v }
        };
        out.push(EventRow {
            id: eid.to_string(),
            name,
            status: "pending".to_string(),
            notes: format!("from graph_事件.json · 事件/{eid}.md"),
        });
    }
    // 特殊节点
    for node in graph
        .get("nodes_special")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
    {
        let Some(eid) = node.get("id").and_then(|v| v.as_str()) else { continue };
        let h1 = node.get("h1").and_then(|v| v.as_str()).unwrap_or(eid);
        let name: String = {
            let re = regex::Regex::new(r"^EVT-\S*\s*[｜·]\s*").unwrap();
            re.replace(h1, "").chars().take(60).collect()
        };
        let note = format!(
            "kind={} · {}",
            node.get("kind").and_then(|v| v.as_str()).unwrap_or(""),
            node.get("note").and_then(|v| v.as_str()).unwrap_or("")
        );
        out.push(EventRow {
            id: eid.to_string(),
            name,
            status: "pending".to_string(),
            notes: note.chars().take(200).collect(),
        });
    }
    out
}

#[cfg(test)]
mod fill_tests {
    use super::*;

    // ───────────── fill.py

    #[test]
    fn outline_chapters_parses() {
        let t = "### 第1章 开局\n正文A\n### 第2章 反转\n正文B\n";
        let ch = outline_chapters(t);
        assert_eq!(ch.len(), 2);
        assert_eq!(ch[0].no, 1);
        assert_eq!(ch[0].title, "开局");
        assert_eq!(ch[1].title, "反转");
    }

    #[test]
    fn extract_foreshadows_needs_recurrence() {
        let t = "### 第1章 A\n引用 FOO-01-001\n### 第2章 B\n无关\n### 第3章 C\nFOO-01-001 再现\n";
        let ch = outline_chapters(t);
        let fs = extract_foreshadows(&ch);
        assert_eq!(fs.len(), 1, "{fs:?}");
        assert_eq!(fs[0].code, "FOO-01-001");
        assert_eq!(fs[0].first_chapter, 1);
        assert_eq!(fs[0].last_chapter, 3);
        assert_eq!(fs[0].count, 2);
        assert!(fs[0].content.contains("第1章"), "{}", fs[0].content);
    }

    #[test]
    fn extract_foreshadows_ignores_single_and_pure_digits() {
        let t = "### 第1章 A\nBAR-02 只出现一次\n12345\n### 第2章 B\n12345\n";
        let ch = outline_chapters(t);
        assert!(extract_foreshadows(&ch).is_empty());
    }

    #[test]
    fn protagonist_seed_shape() {
        let (code, v) = protagonist_seed();
        assert_eq!(code, "CHAR-99-001");
        assert_eq!(v["name"], "陈寂");
        assert!(v["realm"].as_str().unwrap().contains("炼气期"));
    }

    #[test]
    fn world_time_extraction() {
        assert_eq!(world_time_of("汐潮历三年，他出发"), "汐潮历三年");
        assert_eq!(world_time_of("第三纪元初期"), "第三纪元初期");
        assert_eq!(world_time_of("无时间线索"), "");
    }

    #[test]
    fn events_from_graph_mainline_and_special() {
        let g = serde_json::json!({
            "event_cards_mainline": ["EVT-01-001"],
            "nodes_special": [{"id": "EVT-99", "h1": "EVT-99｜特殊节点", "kind": "误类", "note": "n"}]
        });
        let rows = events_from_graph(&g, |eid| Some(format!("# {eid}｜主名")));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "EVT-01-001");
        assert_eq!(rows[0].name, "主名");
        assert_eq!(rows[1].id, "EVT-99");
        assert_eq!(rows[1].name, "特殊节点");
        assert!(rows[1].notes.contains("kind=误类"));
    }
}
