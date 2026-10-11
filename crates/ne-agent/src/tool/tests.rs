//! tool 总线单元测试（从 tool.rs 拆出）。

    use super::*;

    fn ctx(tag: &str) -> (ToolCtx, PathBuf) {
        let d = std::env::temp_dir().join(format!("ne-agent-tool-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        (ToolCtx::new(&d), d)
    }

    #[test]
    fn write_then_read_roundtrip() {
        let (c, d) = ctx("rw");
        let r = execute(&c, Tool::Write, &ToolArgs::new().set("path", "a.txt").set("content", "你好\n第二行"))
            .unwrap();
        assert!(r.ok, "{}", r.output);
        let r = execute(&c, Tool::Read, &ToolArgs::new().set("path", "a.txt")).unwrap();
        assert!(r.ok);
        assert!(r.output.contains("第二行"));
        assert!(r.output.contains("1| "), "read 应带行号");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn edit_requires_unique_match() {
        let (c, d) = ctx("edit");
        execute(&c, Tool::Write, &ToolArgs::new().set("path", "b.txt").set("content", "x\nx\n")).unwrap();
        // 多处匹配 → 明确报错
        let r = execute(&c, Tool::Edit, &ToolArgs::new().set("path", "b.txt").set("old", "x").set("new", "y")).unwrap();
        assert!(!r.ok);
        assert!(r.output.contains("不唯一"), "{}", r.output);
        // 唯一匹配 → 成功
        let r = execute(&c, Tool::Edit, &ToolArgs::new().set("path", "b.txt").set("old", "x\nx").set("new", "z")).unwrap();
        assert!(r.ok, "{}", r.output);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn edit_missing_text_gives_actionable_error() {
        let (c, d) = ctx("editmiss");
        execute(&c, Tool::Write, &ToolArgs::new().set("path", "c.txt").set("content", "abc")).unwrap();
        let r = execute(&c, Tool::Edit, &ToolArgs::new().set("path", "c.txt").set("old", "不存在").set("new", "x")).unwrap();
        assert!(!r.ok);
        assert!(r.output.contains("read"), "错误信息应提示先 read：{}", r.output);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn sandbox_rejects_escape() {
        let (c, d) = ctx("sandbox");
        for bad in ["../outside", "a/../../b", "/etc/passwd"] {
            let r = execute(&c, Tool::Read, &ToolArgs::new().set("path", bad)).unwrap();
            assert!(!r.ok, "{bad} 应被拒绝");
        }
        assert!(c.resolve("../x").is_err());
        assert!(c.resolve("/tmp/x").is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ls_lists_and_marks_dirs() {
        let (c, d) = ctx("ls");
        execute(&c, Tool::Write, &ToolArgs::new().set("path", "sub/f.txt").set("content", "1")).unwrap();
        // 根目录：只应看到 sub/
        let r = execute(&c, Tool::Ls, &ToolArgs::new().set("path", ".")).unwrap();
        assert!(r.ok);
        assert!(r.output.contains("sub/"), "{}", r.output);
        assert!(!r.output.contains("f.txt"), "根目录不该直接列出子目录里的文件：{}", r.output);
        // 子目录：应看到 f.txt
        let r2 = execute(&c, Tool::Ls, &ToolArgs::new().set("path", "sub")).unwrap();
        assert!(r2.ok);
        assert!(r2.output.contains("f.txt"), "{}", r2.output);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn render_handles_cjk_without_corruption() {
        // 回归：早期实现按字节索引 push char，中文被打碎成乱码
        let (c, d) = ctx("cjk");
        execute(&c, Tool::Write, &ToolArgs::new().set("path", "z.txt").set("content", "第一章 残响之河")).unwrap();
        let r = execute(&c, Tool::Read, &ToolArgs::new().set("path", "z.txt")).unwrap();
        assert!(r.ok);
        assert!(r.output.contains("残响之河"), "中文不得乱码：{}", r.output);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_param_reports_clearly() {
        let (c, _d) = ctx("miss");
        let r = execute(&c, Tool::Read, &ToolArgs::new()).unwrap();
        assert!(!r.ok);
        assert!(r.output.contains("path"));
    }

    #[test]
    fn registry_names_are_stable() {
        assert_eq!(Tool::from_name("read"), Some(Tool::Read));
        assert_eq!(Tool::from_name("web_fetch"), Some(Tool::WebFetch));
        assert_eq!(Tool::from_name("plan"), Some(Tool::Plan));
        assert_eq!(Tool::from_name("scratchpad"), Some(Tool::Scratchpad));
        assert_eq!(Tool::from_name("ask_user"), Some(Tool::AskUser));
        assert_eq!(Tool::from_name("load_rule"), Some(Tool::LoadRule));
        assert_eq!(Tool::from_name("load_skill"), Some(Tool::LoadSkill));
        assert_eq!(Tool::from_name("subagents"), Some(Tool::Subagents));
        assert_eq!(Tool::from_name("invoke_subagent"), Some(Tool::Subagents));
        assert_eq!(Tool::from_name("dual_agent"), Some(Tool::DualAgent));
        assert_eq!(Tool::from_name("invoke_dual_agent"), Some(Tool::DualAgent));
        assert_eq!(Tool::from_name("compress"), Some(Tool::Compress));
        assert_eq!(Tool::from_name("compact"), Some(Tool::Compress));
        assert_eq!(Tool::from_name("history_read"), Some(Tool::HistoryRead));
        assert_eq!(Tool::from_name("history_search"), Some(Tool::HistorySearch));
        assert_eq!(Tool::from_name("nope"), None);
        let names: Vec<&str> = Tool::all().iter().map(|t| t.name()).collect();
        assert_eq!(
            names,
            vec![
                "read", "write", "edit", "ls", "base", "web_fetch", "web_search", "use_capability",
                "plan", "scratchpad", "ask_user", "load_rule", "load_skill", "subagents",
                "dual_agent", "compress", "history_read", "history_search"
            ]
        );
    }

    #[test]
    fn capability_aliases_resolve() {
        assert_eq!(Tool::from_name("use_capability"), Some(Tool::Capability));
        assert_eq!(Tool::from_name("capability"), Some(Tool::Capability));
    }

    #[test]
    fn session_local_tools_need_no_approval() {
        // plan/scratchpad 只改会话局部状态
        assert!(Tool::Plan.is_session_local());
        assert!(Tool::Scratchpad.is_session_local());
        assert!(Tool::Compress.is_session_local());
        assert!(Tool::HistoryRead.is_session_local());
        assert!(Tool::HistorySearch.is_session_local());
        assert_eq!(Tool::Plan.tier(), Tier::Read);
        assert_eq!(Tool::Scratchpad.tier(), Tier::Read);
        assert_eq!(Tool::Compress.tier(), Tier::Read);
        assert!(!Tool::Plan.is_network());
    }

    #[test]
    fn plan_tool_roundtrip() {
        let (c, _d) = ctx("plan");
        let cs = crate::context_tools::ContextStore::open(std::env::temp_dir().join(format!(
            "ne-agent-plan-tool-{}.db",
            std::process::id()
        )))
        .unwrap();
        let c = c.with_session("S-plan", cs);
        // 创建
        let a = ToolArgs::new()
            .set("action", "replace_active")
            .set("goal", "写完整卷")
            .set("steps", r#"[{"id":"1"}]"#);
        let r = execute(&c, Tool::Plan, &a).unwrap();
        assert!(r.ok, "{}", r.output);
        assert!(r.output.contains("写完整卷"), "{}", r.output);
        // 读回
        let r = execute(&c, Tool::Plan, &ToolArgs::new().set("action", "get_active")).unwrap();
        assert!(r.ok);
        assert!(r.output.contains("写完整卷"), "{}", r.output);
        // 非法状态不写坏
        let a = ToolArgs::new().set("action", "advance").set("status", "第一阶段完成");
        let r = execute(&c, Tool::Plan, &a).unwrap();
        assert!(r.ok, "{}", r.output);
        assert!(r.output.contains("active"), "非法状态应保留 active：{}", r.output);
        // 清空
        let r = execute(&c, Tool::Plan, &ToolArgs::new().set("action", "clear_active")).unwrap();
        assert!(r.ok);
        let r = execute(&c, Tool::Plan, &ToolArgs::new().set("action", "get_active")).unwrap();
        assert!(r.output.contains("暂无"), "{}", r.output);
    }

    #[test]
    fn plan_tool_without_context_errors_cleanly() {
        let (c, _d) = ctx("plan-noctx");
        let a = ToolArgs::new().set("action", "get_active");
        let r = execute(&c, Tool::Plan, &a).unwrap();
        assert!(!r.ok);
        assert!(r.output.contains("plan"), "{}", r.output);
    }

    #[test]
    fn scratchpad_tool_roundtrip() {
        let (c, _d) = ctx("scratch");
        let cs = crate::context_tools::ContextStore::open(std::env::temp_dir().join(format!(
            "ne-agent-scratch-tool-{}.db",
            std::process::id()
        )))
        .unwrap();
        let c = c.with_session("S-scratch", cs);
        let r = execute(
            &c,
            Tool::Scratchpad,
            &ToolArgs::new().set("action", "save").set("key", "假说").set("value", "DNS 坏了"),
        )
        .unwrap();
        assert!(r.ok, "{}", r.output);
        let r = execute(
            &c,
            Tool::Scratchpad,
            &ToolArgs::new().set("action", "get").set("key", "假说"),
        )
        .unwrap();
        assert!(r.output.contains("DNS 坏了"), "{}", r.output);
        let r = execute(
            &c,
            Tool::Scratchpad,
            &ToolArgs::new().set("action", "list"),
        )
        .unwrap();
        assert!(r.output.contains("假说"), "{}", r.output);
    }

    #[test]
    fn scratchpad_missing_key_is_clear_error() {
        let (c, _d) = ctx("scratch-bad");
        let cs = crate::context_tools::ContextStore::open(std::env::temp_dir().join(format!(
            "ne-agent-scratch-bad-{}.db",
            std::process::id()
        )))
        .unwrap();
        let c = c.with_session("S", cs);
        let r = execute(
            &c,
            Tool::Scratchpad,
            &ToolArgs::new().set("action", "save").set("value", "v"),
        )
        .unwrap();
        assert!(!r.ok);
        assert!(r.output.contains("key"), "{}", r.output);
    }

    #[test]
    fn network_tools_are_read_tier_and_flagged() {
        // 网络工具是只读 tier，但被 is_network 标记（M6 审批用）
        assert_eq!(Tool::WebFetch.tier(), Tier::Read);
        assert_eq!(Tool::WebSearch.tier(), Tier::Read);
        assert!(Tool::WebFetch.is_network());
        assert!(Tool::WebSearch.is_network());
        assert!(!Tool::Read.is_network());
        assert!(!Tool::Base.is_network());
    }

    #[test]
    fn web_search_reports_missing_query() {
        let (c, _d) = ctx("ws");
        let r = execute(&c, Tool::WebSearch, &ToolArgs::new()).unwrap();
        assert!(!r.ok);
        assert!(r.output.contains("query"), "{}", r.output);
    }
