//! L0–L5 六层漏斗的数据模型。
//!
//! 对齐 `05_管线框架/funnel.yaml`（单一事实源）与 `rules.md`（门禁铁律）的真实口径：
//!   * 层 id 是字符串（L0/L1/L1x/L2/L3/L4/L5），不是连续数字 —— L1x 是 L1 的分支层。
//!   * 执行序 order = [L0, L1, L1x, L2, L4, L3, L5]，**L4 必须在 L3 前**（细纲引用需先有卡）。
//!   * 层产物既可能是单文件（file）也可能是目录（dir），二者互斥、至少其一。

use serde::{Deserialize, Serialize};

/// 层标识。用字符串保留 L1x 这类非数字层。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LayerId(pub String);

impl LayerId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Display for LayerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl From<&str> for LayerId {
    fn from(s: &str) -> Self {
        LayerId(s.to_string())
    }
}

/// 层完成态。`status` 在 funnel.yaml 里是自由文本（done / chapters_1_to_3 / pending …），
/// 但语义上只有三类，故解析为枚举并保留原始串（不丢信息）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerState {
    /// 已完成
    Done,
    /// 未开始
    Pending,
    /// 进行中（如章节区间 chapters_1_to_N）
    InProgress { raw: String },
    /// 未识别（原样保留，不臆造语义）
    Unknown { raw: String },
}

impl LayerState {
    /// 便于外部直接构造「未知」态。
    pub fn unknown(raw: &str) -> Self {
        LayerState::Unknown { raw: raw.to_string() }
    }
}

impl LayerState {
    pub fn parse(raw: &str) -> Self {
        let t = raw.trim();
        match t {
            "done" => LayerState::Done,
            "pending" => LayerState::Pending,
            "" => LayerState::Unknown { raw: t.to_string() },
            other if other.starts_with("chapters_") => {
                LayerState::InProgress { raw: other.to_string() }
            }
            other => LayerState::Unknown { raw: other.to_string() },
        }
    }
    pub fn is_done(&self) -> bool {
        matches!(self, LayerState::Done)
    }
    pub fn raw(&self) -> String {
        match self {
            LayerState::Done => "done".into(),
            LayerState::Pending => "pending".into(),
            LayerState::InProgress { raw } => raw.clone(),
            LayerState::Unknown { raw } => raw.clone(),
        }
    }
}

/// 层定义（funnel.yaml `layers[]` 一条）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Layer {
    pub id: LayerId,
    pub name: String,
    /// 产物为单文件时填此
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// 产物为目录时填此
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    /// 该层作用的卷（L2/L3/L4/L5 有）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<String>,
    /// 备注（funnel.yaml 里是行内注释，YAML 解析后丢弃 → 由 raw_status 保留）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub deps: Vec<String>,
}

impl Layer {
    pub fn state(&self) -> LayerState {
        LayerState::parse(&self.status)
    }
    /// 产物路径（单文件或目录），用于存在性校验。
    pub fn artifact(&self) -> Option<&str> {
        self.file.as_deref().or(self.dir.as_deref())
    }
}

/// 执行器规则（funnel.yaml `executor`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Executor {
    /// 真实推进顺序（注意 L4 在 L3 前）
    pub order: Vec<String>,
    #[serde(default)]
    pub skip_done: bool,
    #[serde(default)]
    pub on_blocked: String,
}

/// 完成判据（funnel.yaml `verify`，键名形如 L2_done）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Verify {
    #[serde(flatten)]
    pub criteria: std::collections::BTreeMap<String, String>,
}

/// 资料缺口（funnel.yaml `materials`）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Materials {
    #[serde(default)]
    pub core_standard: Option<u32>,
    #[serde(default)]
    pub per_volume_gap: std::collections::BTreeMap<String, i64>,
    #[serde(default)]
    pub total_core_gap: Option<i64>,
    #[serde(default)]
    pub total_ext_gap: Option<i64>,
}

/// 整份 funnel.yaml 的映射（只取我们消费的键，其余忽略不报错）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Funnel {
    #[serde(default)]
    pub root: String,
    pub layers: Vec<Layer>,
    #[serde(default)]
    pub executor: Option<Executor>,
    #[serde(default)]
    pub verify: Option<Verify>,
    #[serde(default)]
    pub materials: Option<Materials>,
}

impl Funnel {
    pub fn layer(&self, id: &str) -> Option<&Layer> {
        self.layers.iter().find(|l| l.id.as_str() == id)
    }
    /// 执行序：优先用 executor.order；缺失则退化为 layers 的声明顺序（不臆造）。
    pub fn exec_order(&self) -> Vec<String> {
        match &self.executor {
            Some(e) if !e.order.is_empty() => e.order.clone(),
            _ => self.layers.iter().map(|l| l.id.0.clone()).collect(),
        }
    }
}
