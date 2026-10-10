//! funnel.yaml 的读写：加载、产物校验、状态推进。
//!
//! 对齐 Python 侧 `engine/pipeline_backend.py::push_funnel` 的行为：
//! 只替换 L5 块的 status 行，其余**逐字节不动**（不重新序列化整份 YAML，避免注释丢失）。

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::model::Funnel;

/// 一次加载的结果：已解析结构 + 文件是否存在。
pub struct Loaded {
    pub funnel: Funnel,
    pub path: PathBuf,
    pub root: PathBuf,
}

impl Loaded {
    /// 层产物在磁盘上是否真实存在（file 或 dir）。
    pub fn artifact_exists(&self, layer: &crate::model::Layer) -> Option<bool> {
        layer.artifact().map(|rel| self.root.join(rel).exists())
    }
}

/// 从「仓库根」定位 funnel.yaml 并解析。
/// 根目录判据与 Python `_find_root()` 同口径：含 `02_资料库`。
pub fn find_root(start: &Path) -> Option<PathBuf> {
    let mut d: PathBuf = start.to_path_buf();
    for _ in 0..8 {
        if d.join("02_资料库").is_dir() {
            return Some(d);
        }
        if !d.pop() {
            break;
        }
    }
    None
}

pub fn load(root: &Path) -> Result<Loaded> {
    let path = root.join("05_管线框架").join("funnel.yaml");
    let txt = std::fs::read_to_string(&path)
        .with_context(|| format!("读不到 funnel.yaml：{}", path.display()))?;
    let funnel: Funnel = serde_yaml::from_str(&txt)
        .with_context(|| format!("funnel.yaml 解析失败：{}", path.display()))?;
    Ok(Loaded {
        funnel,
        path,
        root: root.to_path_buf(),
    })
}

/// 把 L5 层的 status 推进为 `chapters_1_to_N`。
///
/// 实现口径**刻意与 Python 一致**：正则定位 L5 块后只替换 status 值，
/// 保留文件其余内容（含 # 注释）原样。返回是否发生改动。
pub fn push_l5_chapter(path: &Path, chapter: u32) -> Result<bool> {
    let txt = std::fs::read_to_string(path)?;
    let re = regex::Regex::new(r"(?m)(  - id: L5\n(?:.*\n)*?    status:\s*)(\S+)")?;
    let Some(m) = re.captures(&txt) else {
        return Ok(false);
    };
    let whole = m.get(0).unwrap();
    let prefix = m.get(1).unwrap().as_str();
    let new_block = format!("{prefix}chapters_1_to_{chapter}");
    if whole.as_str() == new_block {
        return Ok(false);
    }
    let mut out = String::with_capacity(txt.len() + 32);
    out.push_str(&txt[..whole.start()]);
    out.push_str(&new_block);
    out.push_str(&txt[whole.end()..]);
    std::fs::write(path, out)?;
    Ok(true)
}
