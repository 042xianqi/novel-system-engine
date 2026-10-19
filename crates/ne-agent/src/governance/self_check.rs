//! 健康自检（world-gov）—— 一键自查治理体系的环境/索引/目录/权限问题。
//!
//! # 铁则
//! - ✅ `--fix` **只做安全修复**：创建缺失目录；**不 ALTER 表、不删任务、不改资料库正文**。
//! - ✅ 只读检查为主；`--json` 输出机器可读报告（可写外部监控）。

use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::Path;

use super::probe_cache;

/// 治理体系必需的子目录（相对库根）。
pub const REQUIRED_SUBDIRS: &[&str] = &[
    "07_勘误工坊",
    "07_勘误工坊/reports",
    "07_勘误工坊/probe_cache",
    "07_勘误工坊/probe_unpack",
];

/// 单条检查结果。
fn ok_item(name: &str, detail: Value) -> Value {
    json!({"name": name, "ok": true, "detail": detail})
}
fn bad_item(name: &str, detail: Value) -> Value {
    json!({"name": name, "ok": false, "detail": detail})
}

/// 运行全部自检。`fix=true` 时**只创建缺失目录**（安全修复）。
pub fn run(conn: &Connection, lib_root: &Path, fix: bool, older_than_days: u64) -> Result<Value> {
    let mut items: Vec<Value> = Vec::new();

    // ① work-root 可访问
    let root_ok = lib_root.is_dir();
    items.push(if root_ok {
        ok_item("work_root", json!({"path": lib_root.display().to_string()}))
    } else {
        bad_item("work_root", json!({"path": lib_root.display().to_string(), "hint": "目录不存在或不可访问"}))
    });

    // ② novel.db / schema 完整（表 + 关键列，含 src_mtime）
    let db_path = lib_root.join("novel.db");
    if db_path.is_file() {
        let mut cols_ok = true;
        let mut missing: Vec<String> = Vec::new();
        let tables = ["library_index", "task_jobs", "risk_index"];
        for t in tables {
            let exists: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    rusqlite::params![t],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            if exists == 0 {
                cols_ok = false;
                missing.push(format!("表 {t}"));
                continue;
            }
            // src_mtime 列检查（增量依赖）
            if t == "library_index" {
                let mut st = conn.prepare("PRAGMA table_info(library_index)")?;
                let cols: Vec<String> = st
                    .query_map([], |r| r.get::<_, String>(1))?
                    .filter_map(|v| v.ok())
                    .collect();
                if !cols.iter().any(|c| c == "src_mtime") {
                    cols_ok = false;
                    missing.push("library_index.src_mtime".to_string());
                }
            }
        }
        items.push(if cols_ok {
            ok_item("schema", json!({"db": db_path.display().to_string(), "tables": tables}))
        } else {
            bad_item("schema", json!({"missing": missing, "hint": "缺表/列；请跑 library-ctl init 或检查库"}))
        });
    } else {
        items.push(bad_item("schema", json!({"db": db_path.display().to_string(), "hint": "novel.db 不存在"})));
    }

    // ③ 必需子目录可创建
    let mut created: Vec<String> = Vec::new();
    let mut dir_ok = true;
    for sub in REQUIRED_SUBDIRS {
        let p = lib_root.join(sub);
        if p.is_dir() {
            continue;
        }
        if fix {
            if std::fs::create_dir_all(&p).is_ok() {
                created.push(sub.to_string());
            } else {
                dir_ok = false;
            }
        } else {
            dir_ok = false;
        }
    }
    items.push(if dir_ok {
        ok_item("subdirs", json!({"required": REQUIRED_SUBDIRS, "created": created}))
    } else {
        bad_item("subdirs", json!({"required": REQUIRED_SUBDIRS, "created": created, "hint": "可加 --fix 创建缺失目录"}))
    });

    // ④ library_index 非空 + src_mtime 有效率（表可能不存在 → 容错，不崩）
    let li_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='library_index'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
        > 0;
    let (total, nonzero): (i64, i64) = if li_exists {
        let t: i64 = conn.query_row("SELECT COUNT(*) FROM library_index", [], |r| r.get(0)).unwrap_or(0);
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM library_index WHERE src_mtime>0", [], |r| r.get(0)).unwrap_or(0);
        (t, n)
    } else {
        (0, 0)
    };
    let ratio = if total > 0 { nonzero as f64 / total as f64 } else { 0.0 };
    let idx_item = if total == 0 {
        bad_item("library_index", json!({"total": 0, "hint": "索引为空；请跑 library-ctl init"}))
    } else if ratio < 0.5 {
        json!({
            "name": "library_index", "ok": true, "warn": true,
            "detail": {"total": total, "src_mtime_nonzero": nonzero, "ratio": ratio,
                "hint": "多数 src_mtime=0 → 很久没跑 library-ctl init，增量扫描会降级"}
        })
    } else {
        ok_item("library_index", json!({"total": total, "src_mtime_nonzero": nonzero, "ratio": ratio}))
    };
    items.push(idx_item);

    // ⑤ 基线法理可加载 + 指纹合法
    match crate::world_gov::baseline_entries(conn) {
        Ok(entries) if !entries.is_empty() => {
            let fp = crate::world_gov::baseline_fingerprint(&entries);
            let fp_ok = !fp.is_empty() && fp.chars().all(|c| c.is_ascii_hexdigit());
            items.push(if fp_ok {
                ok_item("baseline", json!({"entries": entries.len(), "fingerprint": fp}))
            } else {
                bad_item("baseline", json!({"fingerprint": fp, "hint": "指纹非法"}))
            });
        }
        Ok(_) => items.push(bad_item("baseline", json!({"hint": "基线为空；请跑 world-gov bootstrap"}))),
        Err(e) => items.push(bad_item("baseline", json!({"error": e.to_string()}))),
    }

    // ⑥ 管线状态：task_jobs pending/rejected/done 数量 + 卡死提示
    // 表可能尚不存在（未 --persist 过）→ 视为 0，不报错（self-check 本身必须容错）
    let task_table_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='task_jobs'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
        > 0;
    let (pending, done, rejected, stalled_n) = if task_table_exists {
        let p: i64 = conn.query_row("SELECT COUNT(*) FROM task_jobs WHERE state='Pending'", [], |r| r.get(0)).unwrap_or(0);
        let d: i64 = conn.query_row("SELECT COUNT(*) FROM task_jobs WHERE state='Done'", [], |r| r.get(0)).unwrap_or(0);
        let rj: i64 = conn.query_row("SELECT COUNT(*) FROM task_jobs WHERE state='Rejected'", [], |r| r.get(0)).unwrap_or(0);
        let sn = stalled_jobs(conn, 24).map(|v| v.len()).unwrap_or(0);
        (p, d, rj, sn)
    } else {
        (0, 0, 0, 0)
    };
    items.push(json!({
        "name": "pipeline", "ok": stalled_n == 0,
        "detail": {"task_table": task_table_exists, "pending": pending, "done": done, "rejected": rejected,
            "stalled_24h": stalled_n,
            "hint": if !task_table_exists {
                "task_jobs 表未创建（尚未用过 --persist 入队）；非故障"
            } else if stalled_n > 0 {
                "有 >24h 仍 Pending 的任务；用 queue step --dump-stuck 查看"
            } else { "无卡死任务" }}
    }));

    // ⑦ 缓存状态（信息性）
    let cache = probe_cache::status(lib_root, older_than_days).unwrap_or(json!({}));
    items.push(json!({"name": "probe_cache", "ok": true, "detail": cache}));

    // ⑧ 人工标记体系（第 10 批）：item_tag/item_note 表结构 + 脏数据
    let nts = super::note_tag_store::table_check(conn).unwrap_or(json!({"ok": false, "error": "检查失败"}));
    items.push(json!({
        "name": "note_tag_store",
        "ok": nts["ok"] == true,
        "detail": nts,
    }));

    // ⑨ 差异快照（第 11 批）：lib_snapshot 表结构 + 废弃快照提示
    let ds = super::diff_snapshot::table_check(conn, 30).unwrap_or(json!({"ok": false, "error": "检查失败"}));
    items.push(json!({
        "name": "lib_snapshot",
        "ok": ds["ok"] == true,
        "detail": ds,
    }));

    // ⑩ Web 面板依赖可用性（第 12 批；**仅警告，不强制**——纯 std 实现，无常驻依赖）
    items.push(json!({
        "name": "web_panel",
        "ok": true,
        "warn": false,
        "detail": {
            "deps": "std only（零 axum/tokio）",
            "bind": "127.0.0.1",
            "default_port": super::web_gateway::DEFAULT_PORT,
            "note": "面板为按需前台命令；无常驻服务、无开机自启。",
        }
    }));

    // ⑪ 冲突记录库（第 13 批）：表结构 + review_status 脏数据检查（只读）
    let cfs = super::conflict_store::table_check(conn).unwrap_or(json!({"ok": false, "error": "检查失败"}));
    items.push(json!({
        "name": "conflict_records",
        "ok": cfs["ok"] == true,
        "detail": cfs,
    }));

    // ⑫ 关系网（第 14 批）：rel_edge 表结构 + 自环检查（只读）
    let rgs = super::rel_graph::table_check(conn).unwrap_or(json!({"ok": false, "error": "检查失败"}));
    items.push(json!({
        "name": "rel_graph",
        "ok": rgs["ok"] == true,
        "detail": rgs,
    }));
    // ⑫b 索引-实盘一致性（第 2 轮新增）：library_index 指向的文件是否存在。
    //
    // 实测：874 条索引里 **136 条指向不存在的 md**（X 层 115 + L4 层 21）。
    // 这直接导致 `graph build` 报 `unreadable=136`、`inject` 读不到卡正文、
    // 而自检从来不看这一项 → 一直是「全绿但半瞎」。
    // 这里只**报数**不自动删索引（删索引会丢废弃卡的追踪线索）。
    let idx_check = index_vs_disk_check(conn, lib_root);
    items.push(json!({
        "name": "index_vs_disk",
        "ok": idx_check["missing"].as_i64().unwrap_or(0) == 0,
        "detail": idx_check,
    }));

    // ⑬ 沙盘推演（第 15 批）：sandbox_run/sandbox_step 结构 + 步骤数一致性（只读）
    let sbs = super::sandbox::table_check(conn).unwrap_or(json!({"ok": false, "error": "检查失败"}));
    items.push(json!({
        "name": "sandbox",
        "ok": sbs["ok"] == true,
        "detail": sbs,
    }));

    // ⑭ 终态零污染证明（第 16 批）：主表在 + 辅助表名不冲突（只读）
    let ov = super::overview::overview(conn, lib_root).unwrap_or(json!({}));
    let proof = ov.get("no_pollution").cloned().unwrap_or(json!({"ok": false}));
    items.push(json!({
        "name": "no_pollution",
        "ok": proof["ok"] == true,
        "detail": proof,
    }));

    // ⑮ LLM 深度接入（第 17 批）：配置合法性 + 目录权限 + feature/闸门状态（只读，锁死不报错）
    let llm_cfg = super::llm_config::load(lib_root);
    let cfg_ok = llm_cfg.is_ok();
    let cfg_snap = llm_cfg.as_ref().map(super::llm_config::safe_snapshot).unwrap_or(json!({}));
    let cache_dir = lib_root.join(super::inference_cache::CACHE_REL);
    // 目录权限：能创建/访问缓存目录即视为可写；缺失不算错（惰性创建）
    let cache_writable = if cache_dir.is_dir() {
        std::fs::metadata(&cache_dir).map(|m| !m.permissions().readonly()).unwrap_or(false)
    } else {
        // 尝试探测父目录可写（不真正建目录，避免副作用）
        lib_root.join("07_勘误工坊").is_dir()
    };
    let enable = format!(
        "cargo build -p ne-agent --features llm_integration（当前 feature_enabled={}）",
        super::llm_gate::llm_integration_enabled()
    );
    items.push(json!({
        "name": "llm_integration",
        "ok": true,
        "warn": !cfg_ok || !cache_writable,
        "detail": {
            "config_ok": cfg_ok,
            "config_path": lib_root.join(super::llm_config::CONFIG_REL).display().to_string(),
            "config_present": lib_root.join(super::llm_config::CONFIG_REL).is_file(),
            "default_backend": cfg_snap.get("default_backend").cloned().unwrap_or(json!(null)),
            "cache_enabled": cfg_snap.get("cache_enabled").cloned().unwrap_or(json!(true)),
            "cache_dir_writable": cache_writable,
            "feature_enabled": super::llm_gate::llm_integration_enabled(),
            "gate_locked": super::llm_gate::lock_status(false)["gate_locked"],
            "enable_hint": enable,
            "note": "闸门锁死是**默认正常态**，非故障；开启需 feature + WOGOV_ALLOW_REAL_LLM=yes + --real-semantic。",
        }
    }));

    let failed = items.iter().filter(|x| x["ok"] == false).count();
    Ok(json!({
        "command": "world-gov self-check",
        "lib_root": lib_root.display().to_string(),
        "fix": fix,
        "checks": items.len(),
        "failed": failed,
        "all_ok": failed == 0,
        "items": items,
        "toolchain_epoch": 17,
        "note": "自检只读为主；--fix 仅创建缺失目录，绝不 ALTER 表/删任务/改资料库。",
    }))
}

/// 索引 vs 实盘一致性（只读）：`library_index.source` 指向的 md 是否真实存在。
///
/// 为什么必须单独查：
/// `library_index` 有 `source` 列，但**从来没有任何自检去核对文件是否还在**。
/// 实测 874 条索引里 136 条指向不存在的文件（X 层 115 条是废弃卡的死索引，
/// L4 层 21 条是历史改名后没更新 source 的孤儿索引）。
/// 后果：graph build 报 unreadable=136、inject 读卡正文拿空、体检却一片绿。
///
/// 判据与 `rel_graph::build` 完全一致（都用 `lib_root.join(source)` + `is_file`），
/// 否则两个模块会各说各话。
fn index_vs_disk_check(conn: &Connection, lib_root: &Path) -> Value {
    let mut total = 0i64;
    let mut missing_by_layer: std::collections::BTreeMap<String, i64> = Default::default();
    let mut samples: Vec<Value> = Vec::new();
    let mut missing = 0i64;
    let st = conn.prepare("SELECT key,layer,IFNULL(source,'') FROM library_index");
    let Ok(mut st) = st else {
        return json!({"ok": false, "error": "library_index 不可读"});
    };
    let rows = st.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
    });
    let Ok(rows) = rows else {
        return json!({"ok": false, "error": "library_index 查询失败"});
    };
    for row in rows.flatten() {
        let (key, layer, source) = row;
        total += 1;
        if source.is_empty() {
            *missing_by_layer.entry(format!("{layer}:no_source")).or_insert(0) += 1;
            missing += 1;
            continue;
        }
        if !lib_root.join(&source).is_file() {
            *missing_by_layer.entry(layer.clone()).or_insert(0) += 1;
            missing += 1;
            if samples.len() < 10 {
                samples.push(json!({"key": key, "layer": layer, "source": source}));
            }
        }
    }
    json!({
        "total": total,
        "missing": missing,
        "by_layer": missing_by_layer,
        "samples": samples,
        "note": "X 层缺失是废弃卡死索引（可留）；L4 层缺失是历史改名未回写 source（应修）",
    })
}

/// 长期 Pending（> `hours` 小时）任务清单。
pub fn stalled_jobs(conn: &Connection, hours: i64) -> Result<Vec<Value>> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let cutoff = now - hours * 3600;
    let mut st = conn.prepare(
        "SELECT id,kind,state,created_at FROM task_jobs WHERE state='Pending' AND created_at < ?1 ORDER BY created_at",
    )?;
    let rows: Vec<Value> = st
        .query_map(rusqlite::params![cutoff], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "kind": r.get::<_, String>(1)?,
                "state": r.get::<_, String>(2)?,
                "created_at": r.get::<_, i64>(3)?,
            }))
        })?
        .filter_map(|x| x.ok())
        .collect();
    Ok(rows)
}
/// 供 gate-check 的能力标记。
pub fn capabilities() -> Value {
    json!({
        "cache_ready": true,
        "probe_export_ready": true,
        "selfcheck_ready": true,
        "tag_note_ready": true,
        "diff_snap_ready": true,
        "webpanel_ready": true,
        "conflict_store_ready": true,
        "real_semantic_triple_lock": true,
        "rel_graph_ready": true,
        "sandbox_ready": true,
        "llm_integration_ready": true,
        "toolchain_epoch": 17,
    })
}


#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn mem_conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(include_str!("../governance_schema.sql")).unwrap();
        crate::governance::ensure_risk_index_cols(&c).unwrap();
        c
    }

    #[test]
    fn self_check_reports_missing_root_and_dirs() {
        let conn = mem_conn();
        let lib = std::env::temp_dir().join(format!("wg_sc_missing_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&lib);
        let r = run(&conn, &lib, false, 30).unwrap();
        assert_eq!(r["all_ok"], false, "缺目录/库应报失败");
        assert!(r["failed"].as_u64().unwrap() > 0);
        // 不应创建目录
        assert!(!lib.join("07_勘误工坊").exists(), "非 --fix 不得建目录");
    }

    #[test]
    fn fix_creates_only_dirs_not_tables_or_tasks() {
        let conn = mem_conn();
        let lib = std::env::temp_dir().join(format!("wg_sc_fix_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&lib);
        std::fs::create_dir_all(&lib).unwrap();
        // 预置一个任务，确认 --fix 不删它
        crate::governance::pipeline_store::ensure_task_table(&conn).unwrap();
        conn.execute(
            "INSERT INTO task_jobs(id,kind,state,created_at,updated_at) VALUES('j1','InspectCard','Pending',1,1)",
            [],
        ).unwrap();
        let r = run(&conn, &lib, true, 30).unwrap();
        // 目录被创建
        for sub in REQUIRED_SUBDIRS {
            assert!(lib.join(sub).is_dir(), "--fix 应创建 {sub}");
        }
        // 任务未被删
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM task_jobs", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1, "--fix 绝不删任务");
        assert!(r["items"].as_array().unwrap().iter().any(|i| i["name"] == "subdirs"));
        let _ = std::fs::remove_dir_all(&lib);
    }

    #[test]
    fn stalled_jobs_detects_old_pending() {
        let conn = mem_conn();
        crate::governance::pipeline_store::ensure_task_table(&conn).unwrap();
        // 旧任务（created_at=1 → 远古）
        conn.execute("INSERT INTO task_jobs(id,kind,state,created_at,updated_at) VALUES('old','InspectCard','Pending',1,1)", []).unwrap();
        // 新任务（created_at=now+大）
        let future = 9_999_999_999i64;
        conn.execute(&format!("INSERT INTO task_jobs(id,kind,state,created_at,updated_at) VALUES('new','InspectCard','Pending',{future},{future})"), []).unwrap();
        let stalled = stalled_jobs(&conn, 24).unwrap();
        assert_eq!(stalled.len(), 1, "只报 >24h 的旧任务");
        assert_eq!(stalled[0]["id"], "old");
    }

    #[test]
    fn capabilities_marker() {
        let c = capabilities();
        assert_eq!(c["toolchain_epoch"], 17);
        assert_eq!(c["selfcheck_ready"], true);
        assert_eq!(c["tag_note_ready"], true);
        assert_eq!(c["diff_snap_ready"], true);
        assert_eq!(c["webpanel_ready"], true);
        assert_eq!(c["conflict_store_ready"], true);
        assert_eq!(c["real_semantic_triple_lock"], true);
        assert_eq!(c["rel_graph_ready"], true);
        assert_eq!(c["sandbox_ready"], true);
    }
}