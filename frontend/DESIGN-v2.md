# 前端设计契约 v2（2026-10-20 全量重构）

> 所有页面重构必须遵守本文件。目的：五个页面若不定死 token 与交互规范，必然出现五套风格。

## 1. 色板：只用语义 token，禁止硬编码颜色

Tailwind 已把旧 class 名桥接到 CSS 变量（切皮肤零组件改动）。新代码一律用语义名：

| 用途 | class |
|---|---|
| 页面底 | `bg-surface` |
| 卡片 | `bg-surface-card` |
| 悬浮/次级 | `bg-surface-card-2` |
| 描边 / 分割线 | `border-line` / `border-line-divider` |
| 主文字 / 次文字 | `text-content` / `text-content-sec` |
| 主色 | `text-primary` / `bg-primary` / `border-primary` |
| 成功/警告/危险 | `text-success` `bg-warning-soft` `bg-danger-soft` |

旧名仍可用（兼容）：`ink-900/800/700/600`、`fg`、`fg-dim`、`acc`、`ok`、`warn`、`bad`。

- 禁止 `#1e4a6d`、`rgb(30,74,109)` 这类写死颜色（在其它皮肤下会突兀）。
- 透明度用斜杠语法：`bg-primary/12`、`border-primary/30`。

## 2. 质感：只用两档

```css
.glass          /* 轻玻璃：列表项、次级卡片 */
.glass-strong   /* 重玻璃：抽屉、弹窗、浮窗 */
```
两档都已含 `backdrop-filter`，不要再自己写 backdrop-filter（重复叠加会掉帧）。
卡片 hover 抬升用现成的 `.card-cyber`。

## 3. 动效：只准动 transform / opacity

可用类：`.bubble-in`、`.caret`、`animate-fade-in`、`.card-cyber`。
禁止：`transition-all`、动画改 width/height/top/left、长 box-shadow 动画。
移动端硬约束：任何每帧循环必须在无变化时停止，不许 rAF 空转。

## 4. 布局规范

- 主容器 `h-full flex flex-col`，禁止页面级纵向滚动条（内容区自己滚）。
- ≥768px：左侧固定侧边栏 `w-[196px]`，主内容 `flex-1 min-w-0`。
- <768px：侧边栏收起为底部 Dock（`h-[52px]`）。
- 顶部状态栏：胶囊化，高 ≤30px，字号 `text-[11px]`，固定右上，禁止大色块。
- 底部固定元素加 `env(safe-area-inset-bottom)`。

## 5. 五个页面硬性验收点

### ① 对话中枢
- 用户气泡右、AI 气泡左，`.bubble-in` 入场。
- Markdown 至少支持：标题/粗体/行内代码/代码块/列表/链接。
- 代码块：等宽字体、深色底、语言角标、右上「复制」按钮（点击后变「已复制」1.5s）。
- 流式中消息尾部挂 `.caret` 光标闪烁。

### ② 资料库
- 卡片结构：顶部大标题 → 下方彩色 Badge 行（编号/分类/字数）。
- Badge 颜色按分类稳定分配（同分类同色，刷新不变）。
- 操作按钮（查看/编辑）只在 hover / 长按时出现。

### ③ 任务看板
- 禁止显示内部 id（wt-xxxx），用户只认任务名。
- 状态流/时间轴呈现：名称 → 进度 → 结果。
- 报错用醒目框（bg-danger-soft + border-danger/40 + 左侧 3px 竖条），可折叠展开。

### ④ 关系星图
- 点击节点 → 右侧滑出实体详情面板。
- 顶部筛选栏：按实体类型多选过滤。
- 力导向，收敛后停止计算（alpha 衰减到阈值即 done）。
- 有「重新布局」按钮。

### ⑤ 大纲树
- 折叠树：三角、缩进、垂直引导线。
- L0→L5 层级一眼可辨（缩进 + 字号/颜色深浅）。
- 点击展收，支持全部展开/折叠。

## 6. 组件库
已装 `reka-ui@2.11.0`（无头、无样式）。优先用它的 Collapsible / Dialog / Tooltip / Tabs / ScrollArea。
原生实现更简单时就别用库。

## 7. 已有资产（先看再用）
- `src/components/base/`：BaseButton/BaseInput/BaseDialog/BaseSheet/BaseSwitch/BaseToast/BaseCell/BaseBanner/BaseSkeleton/SkinPicker
- `src/style.css`：.glass/.glass-strong/.card-cyber/.bubble-in/.caret
- `src/skin.css`：6 套皮肤变量
- `src/nav.js`：路由状态（纯 UI）；`src/api.js`：后端端点；`src/store.js`：业务状态

## 8. 构建
改完必须在 `frontend/` 跑 `npm run build` 通过（exit 0）。
