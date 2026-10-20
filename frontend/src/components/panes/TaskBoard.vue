<script setup>
// TaskBoard.vue —— 任务看板（v2 重写 · 状态流 / 时间轴）
//
// ══════════════════════════════════════════════════════════════════
// 【2026-10-20 重写口径】对齐 frontend/DESIGN-v2.md §5③ 任务看板
//
//   ① 禁止显示内部任务 id（wt-xxxx）：用户只认任务名。
//      做法：所有对外文本经 stripId() 清洗；id 只保留在 v-for :key、
//      v-memo 签名与 API 调用参数里，绝不渲染。
//   ② 状态流 / 时间轴呈现：任务名 → 进度 → 结果，左侧竖线 + 状态色点。
//   ③ 报错用醒目框：bg-danger-soft + border-danger/40 + 左侧 3px 竖条；
//      长堆栈默认折叠，可展开（v-show / class 切换，不做 height 动画）。
//   ④ 原有功能一个不丢（逐项对照旧文件）：
//        · 顶部四数字卡（排队/运行/失败/已完成）
//        · 执行器在线 + 授权锁状态 + 解锁/上锁（二次确认模态）
//        · 刷新
//        · SSE 实时日志（含空态文案、清空、断线重连、300 条上限、到底才跟随）
//        · 思考过程流式区（可折叠、字符数、3 万字截断提示、自动滚底）
//        · 正文流式区（打字机 + 呼吸光标 + 字符数）
//        · 模型切换/失败/降级 日志
//        · ▶ 执行待处理任务（带队列条数、空队列禁用）
//        · 分组列表 + 渐进渲染（每段 20 条 + 「还有 N 条」）+ v-memo
//        · 行内实时进度（msg / 当前卡名 / done/total / failed）
//        · card_groups 卡名清单（这才是用户要看的「建什么」）
//        · 无卡名时退回 result_preview 摘要（brief）
//        · 任务详情悬浮半屏卡片（Teleport + 底部上滑，含 result_preview）
//        · 空态 / 加载中 / s.err
//      新增（用户在需求里点名、旧版其实没有入口）：
//        · 中止任务（TaskAPI.cancel + 二次确认，避免误杀）
//        · 状态筛选（全部 / 执行中 / 排队 / 失败 / 完成）
//        · 单条任务「派发」（排队任务单独起跑）
//
//   【顺带修掉一个真 bug】旧文件的最后一行 watch：
//     watch(() => dispatching.value, (v) => { if (!v) schedulePoll() })
//   schedulePoll 在整个文件里从未定义（轮询已被 SSE 取代），
//   于是每次派发结束都会抛 ReferenceError，打断后续逻辑。
//   新版删掉这条死引用，派发结束后改为显式 loadTaskBoard(true) + refresh()。
//
//   动效纪律（DESIGN-v2 §3）：只动 transform / opacity。
//   进度条用 scaleX（不再 transition width），折叠用 v-show，不用 max-height 动画。
//   数据仍由 useSystem.js 装配，本组件只消费。
// ══════════════════════════════════════════════════════════════════

import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import { systemData, pushLog, toast } from '../../store.js'
import {
  loadTaskBoard,
  groupTasks,
  loadTaskDetail,
} from '../../composables/useSystem.js'
import { useTasks } from '../../composables/useTasks.js'
// 中止走 api.js 的 TaskAPI（组件不直接 fetch，后端通信统一出口）
import { TaskAPI } from '../../api.js'

const { refresh, dispatch, unlockLLM, lockLLM } = useTasks()
const s = systemData.taskList

// ── 视图状态 ──────────────────────────────────────────────────────
const openId = ref('')            // 当前展开详情的任务（仅作 key/请求参数，不渲染）
const openTask = ref(null)
const detail = ref({})
const busy = ref(false)
const dispatching = ref(false)
/** 状态筛选：all | running | pending | blocked | done */
const filter = ref('all')

// ══════════════════════════════════════════════════════════════════
// 【规范①】内部 id 清洗
//
// 为什么用正则兜底而不是「只是不渲染 t.id」：
//   旧版把 t.id 显式渲染在卡名清单下一行；更糟的是——后端 summary 字段
//   在部分任务（sync / self_check 等）里本身就是 "wt-xxxx" 这种编号，
//   只删模板里的 {{ t.id }} 并不能解决「用户看到编号」的问题。
//   所以对所有进入 UI 的文本统一过滤，从源头保证看板上不出现内部 id。
// ══════════════════════════════════════════════════════════════════
const ID_RE = /\bwt-[0-9a-zA-Z-]{2,}\b/g
function stripId(x) {
  return String(x == null ? '' : x)
    .replace(ID_RE, '')
    .replace(/[（(【[]\s*[）)】\]]/g, '')
    .replace(/[\s·:：,，\-—_/]+/g, ' ')
    .trim()
}

// ── 授权锁（铁则：解锁只能由人点、必须二次确认、流程不得自动 touch 文件）──
const confirming = ref(false)
const lockBusy = ref(false)

function askUnlock() {
  if (confirming.value) return
  confirming.value = true
}

async function confirmUnlock() {
  lockBusy.value = true
  const ok = await unlockLLM()
  lockBusy.value = false
  confirming.value = false
  if (ok) {
    await refresh()
    await loadTaskBoard(true)
  }
}

async function doLock() {
  lockBusy.value = true
  const ok = await lockLLM()
  lockBusy.value = false
  if (ok) {
    await refresh()
    await loadTaskBoard(true)
  }
}

// ── 分组（口径与旧版一致，避免数字对不上）────────────────────────
const groups = computed(() => groupTasks(s.items))
const act = computed(() => s.active || {})

const pendingList = computed(() => groups.value.pending)
const canDispatch = computed(() => pendingList.value.length > 0 && !dispatching.value)

function byTime(list) {
  return [...list].sort((a, b) => (b.created_ts || 0) - (a.created_ts || 0))
}
const running = computed(() => byTime(groups.value.running))
const pending = computed(() => byTime(groups.value.pending))
const done = computed(() => byTime(groups.value.done).slice(0, 40))
const blocked = computed(() => byTime(groups.value.blocked))

// ── 状态色点 + 文字标签（规范②）────────────────────────────────
//   色点类走语义 token：bg-primary / bg-warning / bg-success / bg-danger
const STATE_STYLE = {
  running: { label: '执行中', dot: 'bg-primary', text: 'text-primary' },
  pending: { label: '排队', dot: 'bg-warning', text: 'text-warning' },
  done: { label: '完成', dot: 'bg-success', text: 'text-success' },
  blocked: { label: '阻塞', dot: 'bg-danger', text: 'text-danger' },
  failed: { label: '失败', dot: 'bg-danger', text: 'text-danger' },
  rejected: { label: '拒绝', dot: 'bg-danger', text: 'text-danger' },
}
const UNKNOWN_STYLE = { label: '未知', dot: 'bg-content-disabled', text: 'text-content-sec' }

function st(state) {
  const k = String(state || '').toLowerCase()
  const v = STATE_STYLE[k]
  if (v) return v
  return { ...UNKNOWN_STYLE, label: stripId(state) || '未知' }
}

/**
 * 【2026-10-18 的修正必须保留】把「部分完成」从「失败」里区分出来。
 *   任务表顶层 state 只有 failed/done，而 cardgen 一次派发多张卡时
 *   result 里是 {total, success, failed}。实测 16 条 failed 里 13 条
 *   其实是部分成功，一律标红「失败」是比报错更糟的信息失真。
 *   口径：只对 failed 生效；success > 0 才算部分完成；success=0 是真失败，不美化。
 */
function stOf(t) {
  const raw = String((t && t.state) || '').toLowerCase()
  const base = st(t && t.state)
  if (raw !== 'failed') return base

  let success = 0
  let total = 0
  const pg = t.progress || {}
  if (typeof pg.done === 'number' || typeof pg.total === 'number') {
    const failedN = Number(pg.failed) || 0
    total = Number(pg.total) || 0
    success = Math.max(0, (Number(pg.done) || 0) - failedN)
  }
  if (!success && t.result_preview) {
    try {
      const v = JSON.parse(t.result_preview)
      success = Number(v.success) || success
      total = Number(v.total) || total
    } catch {
      /* result_preview 可能是纯文本（rejected），忽略 */
    }
  }
  if (success > 0) {
    return { label: '部分完成', dot: 'bg-warning', text: 'text-warning', success, total }
  }
  return base
}

/** 修后端时间戳单位（秒 vs 毫秒） */
function fmtTime(ts) {
  if (!ts) return ''
  const ms = ts > 1e12 ? ts : ts * 1000
  const d = new Date(ms)
  const p = (n) => String(n).padStart(2, '0')
  return `${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`
}

/** 任务种类中文名 */
const KIND_LABEL = {
  cardgen: '建卡',
  chapter_gen: '写正文',
  sync: '落库同步',
  check: '一致性检查',
  self_check: '系统自检',
}
function kindName(k) {
  return KIND_LABEL[k] || stripId(k) || '任务'
}

// ══════════════════════════════════════════════════════════════════
// 【规范①核心】任务名解析：用户只认任务名
//   优先级：summary（人话，例：生成 10 张混沌帝国的卡）
//        → card_groups 第一张卡名（例：建卡 · 混沌帝国）
//        → 任务种类中文名（例：写正文）
//   全链路 stripId()，保证任何一层都不会把 wt-xxxx 抛到界面上。
// ══════════════════════════════════════════════════════════════════
function titleOf(t) {
  if (!t) return '未命名任务'
  const nm = stripId(t.summary || t.title || '')
  if (nm) return nm
  const g = (t.card_groups || [])[0]
  const first = g && g.items && g.items.length ? stripId(g.items[0]) : ''
  if (first) return `${kindName(t.kind)} · ${first}`
  return kindName(t.kind)
}

/** 一行结果摘要：优先 result_preview 里最像人话的一段 */
function brief(t) {
  const rp = String((t && t.result_preview) || '').trim()
  if (!rp) return ''
  const m = rp.match(/"(\w+)"\s*:\s*"([^"]{2,60})"/g)
  if (m && m.length) {
    return stripId(
      m
        .slice(0, 3)
        .map((x) => x.replace(/"/g, '').replace(/:/, '='))
        .join(' · ')
    )
  }
  return stripId(rp.slice(0, 160))
}

/** 报错正文（规范③）：失败/阻塞/拒绝 才产出，供醒目框使用 */
function errOf(t) {
  const raw = String((t && t.state) || '').toLowerCase()
  if (!['failed', 'blocked', 'rejected'].includes(raw)) return ''
  const body =
    String((t && t.result_preview) || '').trim() ||
    String((t && t.progress && t.progress.msg) || '').trim() ||
    String((t && t.current) || '').trim()
  const txt = stripId(body)
  return txt || '任务失败，但后端未返回错误详情。'
}
/** 长堆栈才需要「展开 / 收起」 */
function errLong(t) {
  return errOf(t).length > 140
}
const openErr = ref({})
function errOpen(t) {
  return !!openErr.value[t.id]
}
function toggleErr(t) {
  openErr.value[t.id] = !openErr.value[t.id]
}

// ── 进度（规范②：任务名 → 进度 → 结果）────────────────────────
function pct(t) {
  const pg = (t && t.progress) || {}
  const total = Number(pg.total || t.total || 0)
  const doneNv = Number(pg.done || t.done || 0)
  if (!total) return null
  return Math.max(0, Math.min(100, Math.round((doneNv / total) * 100)))
}
function doneN(t) {
  const pg = (t && t.progress) || {}
  return Number(pg.done || t.done || 0)
}
function totalN(t) {
  const pg = (t && t.progress) || {}
  return Number(pg.total || t.total || 0)
}
function failedN(t) {
  const pg = (t && t.progress) || {}
  return Number(pg.failed || t.failed || 0)
}
/** 是否有可展示的进度块 */
function hasProgress(t) {
  const pg = (t && t.progress) || {}
  return !!(pg.msg || pg.current_name || pg.phase) || totalN(t) > 0
}
/** 是否有可展示的结果块 */
function hasResult(t) {
  if (t && t.card_groups && t.card_groups.length) return true
  if (brief(t)) return true
  const raw = String((t && t.state) || '').toLowerCase()
  return raw === 'done' || raw === 'failed'
}
/** 卡名清单里是否真有内容（没内容就别渲染空行） */
function groupItems(g) {
  return (g && g.items ? g.items : []).map((x) => stripId(x)).filter(Boolean)
}

// ══════ 渐进渲染（旧版策略保留）══════
//   · 为什么不真虚拟列表：卡片行高不固定（progress / card_groups 展开后变高），
//     真虚拟化要测量 + 滚动锚定，移动端易抖；
//   · 数据上限受控：后端 task_store::list() 已 clamp(1,500)，done 组再 slice(0,40)。
//   分段揭示（首屏 20 行）+ v-memo（内容没变的行零 diff）手感等同虚拟列表，
//   但没有高度测量风险，也不破坏滚动位置与展开状态。
const VISIBLE_STEP = 20
const shown = ref({ running: VISIBLE_STEP, pending: VISIBLE_STEP, blocked: VISIBLE_STEP, done: VISIBLE_STEP })
function visList(sec) {
  const lim = shown.value[sec.key] ?? VISIBLE_STEP
  return sec.list.length > lim ? sec.list.slice(0, lim) : sec.list
}
function restCount(sec) {
  const lim = shown.value[sec.key] ?? VISIBLE_STEP
  return Math.max(0, sec.list.length - lim)
}
function showMore(key) {
  shown.value[key] = (shown.value[key] ?? VISIBLE_STEP) + VISIBLE_STEP
}

/**
 * 行级 v-memo 依赖签名（全部取原始值）。
 * 必须是原始值：`s.items = r.items` 每次刷新都换新对象，签名里放 t.progress
 * 这类对象引用必变 → v-memo 永远失效，等于没加。
 * updated_ts 兜底：任何 DB 回写都会改它，保证不出现陈旧行。
 */
function memoKey(t) {
  return [
    t.id, t.state, t.summary, t.updated_ts,
    t.result_preview, t.current,
    t.progress?.done, t.progress?.total, t.progress?.failed, t.progress?.msg,
    t.card_groups?.length,
  ]
}

// ══════ 分组 + 筛选（规范② + 原有「分组列表」）══════
const SECTIONS = computed(() => [
  { key: 'running', title: '执行中', tone: 'bg-primary', list: running.value },
  { key: 'pending', title: '排队中', tone: 'bg-warning', list: pending.value },
  { key: 'blocked', title: '失败 / 阻塞', tone: 'bg-danger', list: blocked.value },
  { key: 'done', title: '最近完成', tone: 'bg-success', list: done.value },
])
const FILTERS = computed(() => [
  { key: 'all', label: '全部', dot: '', n: s.items.length || 0 },
  { key: 'running', label: '执行中', dot: 'bg-primary', n: running.value.length },
  { key: 'pending', label: '排队', dot: 'bg-warning', n: pending.value.length },
  { key: 'blocked', label: '失败', dot: 'bg-danger', n: blocked.value.length },
  { key: 'done', label: '完成', dot: 'bg-success', n: done.value.length },
])
/** 只渲染「被选中且非空」的分组 */
const visibleSections = computed(() =>
  SECTIONS.value.filter(
    (x) => (filter.value === 'all' || x.key === filter.value) && x.list.length
  )
)
/** 筛选后一条都没有（用于区分「真没任务」和「该筛选下没任务」） */
const noneAfterFilter = computed(
  () => !visibleSections.value.length && (s.items.length || 0) > 0
)

// ══════ 详情（悬浮半屏卡片，保留）══════
async function toggle(t) {
  if (openId.value === t.id) {
    closeTask()
    return
  }
  openId.value = t.id
  openTask.value = t
  if (detail.value[t.id]) return
  detail.value[t.id] = { loading: true }
  try {
    const r = await loadTaskDetail(t.id)
    detail.value[t.id] = { loading: false, data: r }
  } catch (e) {
    detail.value[t.id] = { loading: false, err: stripId(String(e.message || e)) }
  }
}
function closeTask() {
  openId.value = ''
  openTask.value = null
}
/** 详情正文：同样走 stripId，避免摘要里带内部编号 */
const openResult = computed(() => {
  const d = detail.value[openId.value]
  const raw =
    (d && d.data && d.data.result_preview) ||
    (openTask.value && openTask.value.result_preview) ||
    ''
  const txt = stripId(raw)
  return txt || '（暂无输出）'
})
const openErrText = computed(() => (openTask.value ? errOf(openTask.value) : ''))

async function reload() {
  busy.value = true
  await Promise.all([loadTaskBoard(true), refresh()])
  busy.value = false
  pushLog('任务看板已刷新', 'task')
  toast('已刷新')
}

/**
 * 执行待处理任务（原 ToolPkg 底部按钮迁来的功能，语义不变）。
 *
 * 为什么只派发**一个** id：
 *   后端 dispatch 会 spawn 一个执行器进程，它自己按 limit 扫 pending。
 *   这里取最早一条做入口，并把「当前 pending 总数」作为 limit 一起交出去 ——
 *   否则 19 条任务要点 19 次，谈不上「一键」；而每条各调一次会并发起多个
 *   执行器，互撞单飞锁。传 limit = 队列长度才是正确姿势。
 */
async function runPending() {
  const first = pending.value[0] || pendingList.value[0]
  if (!first) {
    toast('没有待处理任务', true)
    return
  }
  dispatching.value = true
  try {
    const n = Math.max(1, pendingList.value.length || pending.value.length || 1)
    const ok = await dispatch(first.id, n)
    if (ok) {
      // 给执行器一点时间把状态改成 running，然后刷新看板
      setTimeout(async () => {
        await Promise.all([loadTaskBoard(true), refresh()])
      }, 1200)
    }
  } finally {
    dispatching.value = false
  }
}

/** 单条排队任务派发（新增：不想整队列跑时只起一条） */
async function runOne(t) {
  if (!t || dispatching.value) return
  dispatching.value = true
  try {
    const ok = await dispatch(t.id, 1)
    if (ok) setTimeout(async () => { await Promise.all([loadTaskBoard(true), refresh()]) }, 1200)
  } finally {
    dispatching.value = false
  }
}

// ══════ 中止任务（新增；旧版只有 api.js 里的接口，界面上没有入口）══════
const abortTarget = ref(null)
const abortBusy = ref(false)
function askAbort(t) {
  if (abortBusy.value) return
  abortTarget.value = t
}
async function confirmAbort() {
  const t = abortTarget.value
  if (!t) return
  abortBusy.value = true
  try {
    const r = await TaskAPI.cancel(t.id)
    if (r && r.ok === false) throw new Error(r.error || '中止被拒绝')
    pushLog(`已请求中止：${titleOf(t)}`, 'warn')
    toast('已请求中止')
    await Promise.all([loadTaskBoard(true), refresh()])
  } catch (e) {
    toast('中止失败：' + (e && e.message ? e.message : e), true)
  } finally {
    abortBusy.value = false
    abortTarget.value = null
  }
}

/**
 * ══════ 实时通道：SSE（Server-Sent Events）══════
 *
 * 2026-10-17 起彻底废弃 2 秒轮询：
 *   轮询的根本问题不是「2 秒慢」，而是 cardgen 没有更细的进度可轮询——
 *   只有「卡开始」「卡结束」两个时间点，中间 60~150 秒是空的，
 *   改成 100ms 轮询看到的还是空（库里没有新数据）。
 *
 * 现架构：cardgen.py print → exec_pending Popen 逐行读 stdout
 *        → POST /api/tasks/{id}/event → Rust task_events::publish
 *        → SSE 帧推到浏览器。端到端实测 40.9ms。
 * 断线补齐：Rust 侧环形缓冲保留最近 200 条，重连即收到历史；
 *          webui_task.result 继续写（兵底），页面刷新也能 loadTaskBoard 拉回。
 */
const sseConnected = ref(false)
const logBox = ref(null)
const sseLogs = ref([])
const sseError = ref('')
let es = null
let reconnectTimer = null
let logSeq = 0
const MAX_LOGS = 300

// ══════════════════════════════════════════════════════════════════
// 全流式：思考过程 + 正文 双通道
//   · sseLogs  → 程序日志（「⚠ 模型 502，正在重试」/「✅ 已落盘」）
//   · thinking → 模型自己的思考
//   · content  → 模型正在写的正文（逐字打字机）
//
// 为什么用节流缓冲而不是直接 push：
//   实测一张卡 150s 吐数百~数千 chunk，每 chunk 触发 Vue 响应式更新会把
//   主线程打死（1000 次更新排下来必然掉帧）。做法：chunk 先拼进非响应式
//   buffer，50ms timer 统一 flush 一次。1000 次更新压成 20 次，依旧丝滑。
// ══════════════════════════════════════════════════════════════════
const thinkingText = ref('')
const contentText = ref('')
const streamCard = ref(null)
const thinkOpen = ref(true)      // 思考区默认展开（2026-10-18 用户要求常驻可读）
const streamOpen = ref(true)
const modelNow = ref('')
const thinkDone = ref(false)

// 模板 ref 必须用 ref() 声明。旧版曾写成 `let thinkBox = null`，裸 let 不被
// setup 返回的代理包装 → 永远 null → 自动滚动从未生效（用户反馈过这个 bug）。
const thinkBox = ref(null)
const contentBox = ref(null)

let bufThink = ''
let bufContent = ''
const bufDirty = ref({ think: false, content: false })
// 持续态标志：管「这张卡此刻是不是还在吐字」（UI 用）。
// 不能用 bufDirty 驱动指示器——它是节流脏标记，50ms 就置回 false，
// 挂在它上面的 v-if 会带着文字每秒跳二十来次 = 闪烁。
const thinkActive = ref(false)
const contentActive = ref(false)
let flushTimer = null
const FLUSH_MS = 50
const MAX_THINK = 30000          // 思考上限 3 万字（超出截断，防内存膨胀）

/** 平滑滚底：连续 smooth 请求会被浏览器合并成动画，观感是「持续跟随」而不是抽搐 */
function autoScroll(el) {
  if (!el) return
  try {
    el.scrollTo({ top: el.scrollHeight, behavior: 'smooth' })
  } catch (e) {
    el.scrollTop = el.scrollHeight
  }
}

function scheduleFlush() {
  if (flushTimer) return
  flushTimer = setTimeout(() => {
    flushTimer = null
    if (bufDirty.value.think) {
      bufDirty.value.think = false
      thinkingText.value = bufThink.slice(-MAX_THINK)
      // 只在展开时滚：收起时容器 display:none，scrollHeight=0，滚了也没意义
      if (thinkOpen.value) autoScroll(thinkBox.value)
    }
    if (bufDirty.value.content) {
      bufDirty.value.content = false
      contentText.value = bufContent
      if (streamOpen.value) autoScroll(contentBox.value)
    }
  }, FLUSH_MS)
}

/** 重置一张卡的流式状态（新卡开始时调用） */
function resetStream(card) {
  bufThink = ''
  bufContent = ''
  bufDirty.value = { think: false, content: false }
  thinkActive.value = false
  contentActive.value = false
  thinkingText.value = ''
  contentText.value = ''
  thinkOpen.value = true
  thinkDone.value = false
  streamCard.value = card || null
}

/** 收尾：把缓冲区剩余内容强制刷进 ref（卡结束时调用，否则末尾几个字看不到） */
function flushNow() {
  if (flushTimer) { clearTimeout(flushTimer); flushTimer = null }
  bufDirty.value = { think: false, content: false }
  thinkingText.value = bufThink.slice(-MAX_THINK)
  contentText.value = bufContent
}

/**
 * 【空态常驻】面板显示开关
 *   用户报过：重启后打开看板，「实时日志 / 思考过程」两个框完全消失，
 *   界面像死水。根因不是 DOM 被删，而是条件是纯数据驱动：
 *   sseLogs 空（环形缓冲只留 200 条）、streamCard 为 null（执行器已死）。
 *   而「任务在跑但什么反馈都没有」时，用户无法判断该等还是该动手 ——
 *   空态必须显性告知「为什么空」。所以只要有 running 任务，两个框常驻。
 */
const hasRunning = computed(() => (groups.value.running || []).length > 0)
const showLogBox = computed(() => sseLogs.value.length > 0 || !!sseError.value || hasRunning.value)
const showThinkBox = computed(() => !!thinkingText.value || !!streamCard.value || hasRunning.value)
const showContentBox = computed(() => !!contentText.value || !!streamCard.value || hasRunning.value)

/** 追加一条日志（带上限，防跑 80 张卡把内存吃满） */
function pushSseLog(entry) {
  const box = logBox.value
  // 只有用户已经在底部时才自动跟随；向上翻了就不抢滚动位置
  const atBottom = box ? (box.scrollHeight - box.scrollTop - box.clientHeight) < 60 : true
  sseLogs.value.push({ seq: ++logSeq, ...entry })
  if (sseLogs.value.length > MAX_LOGS) {
    sseLogs.value.splice(0, sseLogs.value.length - MAX_LOGS)
  }
  if (atBottom) {
    requestAnimationFrame(() => {
      const b = logBox.value
      if (b) b.scrollTop = b.scrollHeight
    })
  }
}

/** 处理一条 SSE 事件 */
function handleSseEvent(ev) {
  const type = ev.type || ''
  const d = ev.data || {}
  if (type === 'log') {
    pushSseLog({ ts: ev.ts || Date.now(), line: d.line || '', stream: d.stream || 'stdout' })
    return
  }
  if (type === 'task_state') {
    const list = (s && s.items) || []
    const t = list.find((x) => x.id === d.id)
    if (t) {
      t.state = d.state || t.state
      t.progress = {
        msg: d.msg || '', current: d.current || '', current_name: d.current_name || '',
        current_category: d.current_category || '', phase: d.phase || '',
        done: d.done || 0, total: d.total || 0, failed: d.failed || 0,
        results: d.results || [],
      }
      if (d.state === 'done' || d.state === 'failed' || d.state === 'rejected') {
        // 终态：拉一次全量（result 里有完整汇总）。
        // 同时收掉「思考中 / 正在输出」的呼吸点：任务被 kill 或异常结束时
        // card_done/card_fail 可能永远不来，呼吸点会一直闪，看着像「卡还在跑」。
        thinkActive.value = false
        contentActive.value = false
        loadTaskBoard(true)
      }
    }
    return
  }
  if (type === 'card_start') {
    // 新卡开始 → 清空上一条的思考/正文，避免两卡的 token 混在一起
    resetStream({ code: d.cardId, name: stripId(d.name), category: stripId(d.category), idx: d.idx, total: d.total })
    pushSseLog({ ts: ev.ts || Date.now(), line: '▶ [' + (d.idx || '?') + '/' + (d.total || '?') + '] 开始生成 ' + stripId(d.name) + '（' + stripId(d.category) + '）', stream: 'card' })
    return
  }
  // 模型思考 token：高频，绝不直接改 ref → 走 buffer + 50ms 节流
  if (type === 'thinking') {
    if (d.text) {
      bufThink += d.text
      bufDirty.value.think = true
      thinkActive.value = true
      scheduleFlush()
    }
    return
  }
  // 模型正文 token（逐字打字机）
  if (type === 'content') {
    if (d.text) {
      bufContent += d.text
      bufDirty.value.content = true
      contentActive.value = true
      scheduleFlush()
    }
    return
  }
  if (type === 'model_start') {
    modelNow.value = stripId(d.model)
    pushSseLog({ ts: ev.ts || Date.now(), line: '🤖 模型 ' + stripId(d.model) + '（第 ' + (d.attempt || 1) + ' 次尝试）', stream: 'card' })
    return
  }
  if (type === 'model_fail') {
    pushSseLog({ ts: ev.ts || Date.now(), line: '⚠ 模型 ' + stripId(d.model) + ' 第 ' + (d.attempt || 1) + ' 次失败：' + stripId(d.why), stream: 'card' })
    return
  }
  if (type === 'model_fallback') {
    modelNow.value = stripId(d.to)
    pushSseLog({ ts: ev.ts || Date.now(), line: '↓ 降级：' + stripId(d.frm || '?') + ' → ' + stripId(d.to || '?'), stream: 'card' })
    return
  }
  if (type === 'card_done') {
    // 思考阶段结束：标记「完成」但不自动收起（用户要求思考框常驻可读）
    thinkDone.value = true
    bufDirty.value.think = false
    contentActive.value = false
    thinkActive.value = false
    flushNow()
    pushSseLog({ ts: ev.ts || Date.now(), line: '✅ [' + (d.idx || '?') + '/' + (d.total || '?') + '] ' + stripId(d.name) + ' → ' + (d.chars || 0) + ' 字', stream: 'card' })
    return
  }
  if (type === 'card_fail') {
    thinkDone.value = true
    bufDirty.value.think = false
    contentActive.value = false
    thinkActive.value = false
    flushNow()
    pushSseLog({ ts: ev.ts || Date.now(), line: '❌ [' + (d.idx || '?') + '/' + (d.total || '?') + '] ' + stripId(d.name) + ' 失败：' + stripId(d.why) + ' ' + (d.probs || []).map(stripId).join('; '), stream: 'card' })
    return
  }
}

/** 建立 SSE 连接（含断线重连：EventSource 默认 3s 重试 + 后端环形缓冲补历史） */
function connectSse() {
  if (es) { try { es.close() } catch (e) { /* noop */ } es = null }
  try {
    es = new EventSource('/api/tasks/stream')
  } catch (e) {
    sseError.value = 'EventSource 不可用'
    return
  }
  es.onopen = () => { sseConnected.value = true; sseError.value = '' }
  es.onmessage = (m) => {
    try {
      handleSseEvent(JSON.parse(m.data))
    } catch (e) { /* 忽略脏帧 */ }
  }
  es.onerror = () => {
    sseConnected.value = false
    sseError.value = '连接中断，3 秒后重连…'
    try { es.close() } catch (e) { /* noop */ }
    es = null
    if (reconnectTimer) clearTimeout(reconnectTimer)
    reconnectTimer = setTimeout(connectSse, 3000)
  }
}

onMounted(async () => {
  await loadTaskBoard()
  connectSse()
})
onUnmounted(() => {
  if (reconnectTimer) clearTimeout(reconnectTimer)
  reconnectTimer = null
  if (es) { try { es.close() } catch (e) { /* noop */ } es = null }
  if (flushTimer) { clearTimeout(flushTimer); flushTimer = null }
})
// 一按「执行」立刻重连一次，确保不漏掉开头几条事件。
// （旧版这里还挂了第二条 watch 调 schedulePoll()，而该函数从未定义 —— 派发
//   结束必抛 ReferenceError。轮询已被 SSE 取代，直接删掉这条死引用，
//   刷新交由 runPending / runOne 里的显式 loadTaskBoard(true) 负责。）
watch(() => dispatching.value, (v) => { if (!v) connectSse() })
</script>

<template>
  <div
    class="h-full overflow-y-auto px-3 py-3 space-y-3"
    :style="{ paddingBottom: 'calc(14px + env(safe-area-inset-bottom, 0px))' }"
  >
    <!-- ══════ 顶部数字概览（原有）══════ -->
    <div class="grid grid-cols-4 gap-2">
      <div class="glass rounded-md px-2 py-2 text-center">
        <div class="text-[18px] font-semibold text-warning leading-none">{{ act.pending ?? 0 }}</div>
        <div class="text-[10px] text-content-sec mt-1">排队</div>
      </div>
      <div class="glass rounded-md px-2 py-2 text-center">
        <div class="text-[18px] font-semibold text-primary leading-none flex items-center justify-center gap-1">
          <span v-if="(act.running ?? 0) > 0" class="inline-block w-1.5 h-1.5 rounded-full bg-primary animate-dotbreathe" />
          {{ act.running ?? 0 }}
        </div>
        <div class="text-[10px] text-content-sec mt-1">运行</div>
      </div>
      <div class="glass rounded-md px-2 py-2 text-center">
        <div class="text-[18px] font-semibold text-danger leading-none">{{ act.blocked ?? 0 }}</div>
        <div class="text-[10px] text-content-sec mt-1">失败</div>
      </div>
      <div class="glass rounded-md px-2 py-2 text-center">
        <div class="text-[18px] font-semibold text-success leading-none">{{ act.done ?? 0 }}</div>
        <div class="text-[10px] text-content-sec mt-1">已完成</div>
      </div>
    </div>

    <!-- ══════ 执行器 / 授权锁 状态胶囊（原有，做成 ≤30px 胶囊）══════ -->
    <div class="glass rounded-md px-3 py-2 space-y-2">
      <div class="flex items-center gap-2 flex-wrap min-h-[30px] text-[11px]">
        <span class="inline-flex items-center gap-1.5 px-2 py-1 rounded-full border border-line bg-surface-card-2">
          <span class="w-1.5 h-1.5 rounded-full" :class="act.executorAlive ? 'bg-success' : 'bg-content-disabled'" />
          <span :class="act.executorAlive ? 'text-success' : 'text-content-sec'">
            执行器{{ act.executorAlive ? '在线' : '空闲' }}
          </span>
          <span v-if="act.executorPid" class="font-mono text-content-sec/60">#{{ act.executorPid }}</span>
        </span>
        <span class="inline-flex items-center gap-1.5 px-2 py-1 rounded-full border border-line bg-surface-card-2">
          <span class="w-1.5 h-1.5 rounded-full" :class="act.llmUnlocked ? 'bg-success' : 'bg-danger'" />
          <span :class="act.llmUnlocked ? 'text-success' : 'text-danger'">
            {{ act.llmUnlocked ? '已解锁' : '已上锁' }}
          </span>
        </span>
        <span v-if="modelNow" class="inline-flex items-center gap-1.5 px-2 py-1 rounded-full border border-line bg-surface-card-2 max-w-[160px]">
          <span class="text-[10px] text-content-sec">模型</span>
          <span class="text-primary truncate">{{ modelNow }}</span>
        </span>

        <!-- 授权锁开关：解锁必须二次确认、只能由人点（铁则保留） -->
        <button
          class="ml-auto shrink-0 px-2 py-1 rounded-full text-[10.5px] font-medium border
                 active:scale-95 disabled:opacity-40 transition-colors"
          :class="act.llmUnlocked ? 'bg-danger-soft text-danger border-danger/30' : 'bg-success-soft text-success border-success/30'"
          :disabled="lockBusy"
          :title="act.llmUnlocked ? '点击上锁，禁止真实外呼' : '点击解锁，允许真实外呼'"
          @click="act.llmUnlocked ? doLock() : askUnlock()"
        >{{ lockBusy ? '…' : (act.llmUnlocked ? '🔓 上锁' : '🔒 解锁') }}</button>
        <button
          class="shrink-0 px-2 py-1 rounded-full border border-line text-content-sec
                 text-[10.5px] active:scale-95 disabled:opacity-40 transition-colors"
          :disabled="busy"
          @click="reload"
        >{{ busy ? '刷新中…' : '刷新' }}</button>
      </div>

      <!-- ── 实时日志（SSE，原有）── -->
      <div v-if="showLogBox" class="rounded-md border border-line/60 overflow-hidden">
        <div class="flex items-center gap-2 px-2.5 py-1.5 bg-surface-card-2">
          <span class="w-1.5 h-1.5 rounded-full shrink-0" :class="sseConnected ? 'bg-success' : 'bg-danger'" />
          <span class="text-[10.5px] text-content-sec">实时日志</span>
          <span class="text-[9.5px] text-content-sec/60 font-mono">{{ sseLogs.length }}</span>
          <span v-if="sseError" class="text-[9.5px] text-warning ml-auto">{{ sseError }}</span>
          <button v-else-if="sseLogs.length" class="text-[9.5px] text-content-sec/70 ml-auto" @click="sseLogs = []">清空</button>
          <span v-else class="text-[9px] text-content-sec/50 ml-auto">{{ sseConnected ? '已连接，等待事件…' : '未连接' }}</span>
        </div>
        <div ref="logBox" class="max-h-56 overflow-y-auto px-2.5 py-1.5 space-y-0.5 bg-surface/60 scroll-follow">
          <TransitionGroup name="tb-log">
            <div
              v-for="l in sseLogs"
              :key="l.seq"
              class="text-[10px] leading-relaxed font-mono whitespace-pre-wrap break-all"
              :class="{
                'text-success/90': l.line.includes('✅'),
                'text-warning/90': l.line.includes('⚠'),
                'text-danger/90': l.line.includes('❌'),
                'text-content-sec': !l.line.includes('✅') && !l.line.includes('⚠') && !l.line.includes('❌'),
              }"
            >{{ l.line }}</div>
          </TransitionGroup>
          <!-- 空态：有任务在跑但没有任何日志 —— 必须明说，否则用户以为界面死了 -->
          <div v-if="!sseLogs.length" class="text-[10px] text-content-sec/55 italic leading-relaxed">
            暂无日志事件。<br>
            有任务在运行却没输出，通常是执行器进程已中断（App 重启会杀掉它）。
            点下方「▶ 执行待处理任务」可重新拉起。
          </div>
        </div>
      </div>

      <!-- ── 思考过程（流式，可折叠，原有）── -->
      <div v-if="showThinkBox" class="rounded-md border border-line/60 overflow-hidden">
        <button class="w-full flex items-center gap-2 px-2.5 py-1.5 bg-surface-card-2 text-left" @click="thinkOpen = !thinkOpen">
          <span class="text-[11px] shrink-0">🧠</span>
          <span class="text-[10.5px] text-content-sec shrink-0">思考过程</span>
          <span v-if="thinkActive && !thinkDone" class="w-1.5 h-1.5 rounded-full bg-primary shrink-0 animate-dotbreathe" title="模型正在思考" />
          <span v-if="thinkDone" class="text-[9px] text-success/70 shrink-0">完成</span>
          <span class="text-[9.5px] text-content-sec/60 font-mono shrink-0">{{ thinkingText.length ? thinkingText.length + ' 字' : '' }}</span>
          <span class="ml-auto tb-arrow text-content-sec/60 shrink-0" :class="{ 'is-open': thinkOpen }">›</span>
        </button>
        <div v-show="thinkOpen" ref="thinkBox" class="max-h-56 overflow-y-auto px-2.5 py-1.5 bg-surface/60 scroll-follow">
          <div v-if="!thinkingText" class="text-[10px] text-content-sec/50 italic">
            {{ hasRunning ? '有任务在运行，但当前卡片尚未产生思考输出（执行器可能已中断）' : '（该模型不输出思考过程，或尚未开始）' }}
          </div>
          <div v-else class="text-[10px] leading-relaxed font-mono whitespace-pre-wrap break-all text-content-sec/80">{{ thinkingText }}</div>
          <div v-if="thinkingText.length >= MAX_THINK" class="mt-1 text-[9px] text-warning/70">
            ⚠ 思考过程过长，已折叠较早内容（仅保留最近 {{ MAX_THINK }} 字）
          </div>
        </div>
      </div>

      <!-- ── 正文输出（打字机，原有）── -->
      <div v-if="showContentBox" class="rounded-md border border-line/60 overflow-hidden">
        <button class="w-full flex items-center gap-2 px-2.5 py-1.5 bg-surface-card-2 text-left" @click="streamOpen = !streamOpen">
          <span class="text-[11px] shrink-0">✍️</span>
          <span class="text-[10.5px] text-content-sec shrink-0">正文输出</span>
          <span v-if="contentActive" class="w-1.5 h-1.5 rounded-full bg-success shrink-0 animate-dotbreathe" title="正在写入正文" />
          <span class="text-[9.5px] text-content-sec/60 font-mono shrink-0">{{ contentText.length }} 字</span>
          <span class="ml-auto tb-arrow text-content-sec/60 shrink-0" :class="{ 'is-open': streamOpen }">›</span>
        </button>
        <div v-show="streamOpen" ref="contentBox" class="max-h-56 overflow-y-auto px-2.5 py-1.5 bg-surface/60 scroll-follow">
          <div v-if="!contentText" class="text-[10px] text-content-sec/50 italic">
            {{ hasRunning ? '有任务在运行，但当前卡片尚未开始写正文' : '（尚未产生正文输出）' }}
          </div>
          <span v-else class="text-[10.5px] leading-relaxed whitespace-pre-wrap break-words text-content/85">{{ contentText }}<span v-if="contentActive" class="tb-caret" /></span>
        </div>
      </div>

      <!-- 执行待处理任务（原 ToolPkg 全局按钮，功能保留） -->
      <div class="flex items-center gap-2">
        <button
          class="flex-1 rounded-md py-2 text-[12px] font-medium ripple-host
                 bg-primary/12 text-primary border border-primary/30
                 active:scale-[0.98] disabled:opacity-35 transition-colors"
          :class="canDispatch ? 'glow-btn' : ''"
          :disabled="!canDispatch"
          @click="runPending"
        >{{ dispatching ? '派发中…' : '▶ 执行待处理任务' }}</button>
        <span class="shrink-0 text-[10px] text-content-sec/70 font-mono">
          {{ pending.length ? `${pending.length} 条排队` : '队列空' }}
        </span>
      </div>
      <div class="text-[10px] text-content-sec/50 leading-relaxed">
        派发后由后端 spawn 执行器按队列消费；真跑需先解锁授权。
      </div>
    </div>

    <!-- ── 数据错误（醒目框，规范③）── -->
    <div v-if="s.err" class="tb-err">
      <span class="tb-err-bar" aria-hidden="true" />
      <div class="tb-err-body">
        <div class="tb-err-head">
          <span class="tb-err-title">⚠ 看板数据拉取失败</span>
        </div>
        <pre class="tb-err-text is-open">{{ stripId(s.err) }}</pre>
      </div>
    </div>

    <!-- ── 状态筛选（原有分组 + 新增筛选）── -->
    <div class="flex items-center gap-1.5 overflow-x-auto tb-noscroll" role="tablist" aria-label="按状态筛选">
      <button
        v-for="f in FILTERS"
        :key="f.key"
        role="tab"
        :aria-selected="filter === f.key"
        class="shrink-0 inline-flex items-center gap-1.5 px-2.5 py-1 rounded-full border text-[10.5px]
               transition-transform active:scale-95"
        :class="filter === f.key
          ? 'bg-primary/12 border-primary/35 text-primary'
          : 'bg-surface-card-2 border-line text-content-sec'"
        @click="filter = f.key"
      >
        <span v-if="f.dot" class="w-1.5 h-1.5 rounded-full" :class="f.dot" />
        <span>{{ f.label }}</span>
        <span class="font-mono text-[9.5px] opacity-70">{{ f.n }}</span>
      </button>
    </div>

    <!-- ══════ 时间轴（规范②：任务名 → 进度 → 结果）══════ -->
    <section v-for="sec in visibleSections" :key="sec.key" class="space-y-2">
      <div class="flex items-center gap-2 px-1">
        <span class="w-1 h-3 rounded-full" :class="sec.tone" />
        <span class="text-[11px] text-content-sec">{{ sec.title }}</span>
        <span class="text-[10px] text-content-sec/60 font-mono">{{ sec.list.length }}</span>
      </div>

      <ol class="tb-timeline">
        <li v-for="t in visList(sec)" :key="t.id" v-memo="memoKey(t)" class="tb-node">
          <!-- 状态色点（规范②） -->
          <span
            class="tb-dot"
            :class="[stOf(t).dot, t.state === 'running' ? 'animate-dotbreathe' : '']"
            aria-hidden="true"
          />
          <div class="glass card-cyber overflow-hidden">
            <!-- ① 任务名（用户只认名字；内部 id 不渲染） -->
            <button
              class="w-full text-left px-3 py-2 flex items-center gap-2 active:bg-surface-card-2"
              @click="toggle(t)"
            >
              <span class="shrink-0 inline-flex items-center gap-1 text-[10px] font-medium" :class="stOf(t).text">
                <span class="w-1.5 h-1.5 rounded-full" :class="stOf(t).dot" />
                {{ stOf(t).label }}
              </span>
              <span class="flex-1 min-w-0 text-[11.5px] text-content truncate" :title="titleOf(t)">
                {{ titleOf(t) }}
              </span>
              <!-- 部分成功：成了几张 / 坏了几张，不点开也看得见 -->
              <span v-if="stOf(t).success > 0" class="shrink-0 text-[10px] font-mono text-success/90">
                ✓{{ stOf(t).success }}<span v-if="stOf(t).total - stOf(t).success > 0" class="text-danger/80"> / ✗{{ stOf(t).total - stOf(t).success }}</span>
              </span>
              <span class="shrink-0 text-[10px] text-content-sec/70 font-mono">{{ fmtTime(t.created_ts) }}</span>
              <span class="shrink-0 text-[10px] text-content-sec" aria-hidden="true">{{ openId === t.id ? '▴' : '▾' }}</span>
            </button>

            <!-- ② 进度：执行中必须能看到「正在生成 XX」 + 百分比条（scaleX，不动 width） -->
            <div v-if="hasProgress(t) && openId !== t.id" class="px-3 pb-1.5 -mt-1 space-y-1">
              <div class="flex items-center gap-1.5">
                <span class="w-1.5 h-1.5 rounded-full shrink-0" :class="t.state === 'running' ? 'bg-primary animate-dotbreathe' : 'bg-content-disabled'" />
                <span class="text-[10.5px] truncate" :class="t.state === 'running' ? 'text-primary' : 'text-content-sec'">
                  {{ stripId(t.progress?.msg || t.progress?.phase || (t.progress?.current_name ? '正在生成 ' + t.progress.current_name : '')) || '已完成' }}
                </span>
                <span v-if="pct(t) !== null" class="ml-auto shrink-0 text-[9.5px] font-mono text-content-sec/80">{{ pct(t) }}%</span>
              </div>
              <div v-if="pct(t) !== null" class="h-1 rounded-full bg-line/60 overflow-hidden">
                <div
                  class="h-full origin-left bg-primary transition-transform duration-500"
                  :style="{ transform: `scaleX(${pct(t) / 100})` }"
                />
              </div>
              <div v-if="totalN(t) > 0" class="flex items-center gap-2 text-[9.5px] text-content-sec/80 font-mono">
                <span>{{ doneN(t) }} / {{ totalN(t) }}</span>
                <span v-if="failedN(t) > 0" class="text-danger">失败 {{ failedN(t) }}</span>
              </div>
            </div>

            <!-- ③ 结果：卡名清单（用户要看的「建什么」）/ 摘要 -->
            <div v-if="hasResult(t) && openId !== t.id && !errOf(t)" class="px-3 pb-2 -mt-0.5 space-y-0.5">
              <div class="flex items-center gap-1.5">
                <span class="text-[9px] px-1 py-0.5 rounded-sm bg-surface-card-2 text-content-sec border border-line">结果</span>
                <span v-if="t.card_groups && t.card_groups.length" class="text-[10px] text-content-sec/70">{{ t.card_groups.length }} 组</span>
              </div>
              <template v-if="t.card_groups && t.card_groups.length">
                <div v-for="(g, gi) in t.card_groups" :key="gi" class="text-[10.5px] leading-relaxed">
                  <span class="text-primary/80">{{ stripId(g.kind_cn) }}</span>
                  <template v-if="groupItems(g).length">
                    <span class="text-content-sec"> · </span>
                    <span v-for="(nm, ni) in groupItems(g)" :key="ni" class="text-content/85">{{ nm }}<span v-if="ni < groupItems(g).length - 1" class="text-content-sec/50">、</span></span>
                  </template>
                  <span v-else class="text-content-sec/60">（{{ g.count }} 张，卡名未指定）</span>
                </div>
              </template>
              <div v-else-if="brief(t)" class="text-[10.5px] text-content-sec leading-relaxed line-clamp-2">{{ brief(t) }}</div>
              <div v-else class="text-[10.5px] text-content-sec/60">已生成，无结果摘要。</div>
            </div>

            <!-- ④ 报错醒目框（规范③）：bg-danger-soft + border-danger/40 + 左侧 3px 竖条 -->
            <div v-if="errOf(t) && openId !== t.id" class="px-3 pb-2 -mt-0.5">
              <div class="tb-err">
                <span class="tb-err-bar" aria-hidden="true" />
                <div class="tb-err-body">
                  <div class="tb-err-head">
                    <span class="tb-err-title">✕ {{ stOf(t).label }}</span>
                    <button
                      v-if="errLong(t)"
                      class="tb-err-toggle"
                      @click="toggleErr(t)"
                    >{{ errOpen(t) ? '收起堆栈' : '展开堆栈' }}</button>
                  </div>
                  <pre
                    class="tb-err-text"
                    :class="errOpen(t) ? 'is-open' : 'is-clamped'"
                  >{{ errOf(t) }}</pre>
                </div>
              </div>
            </div>

            <!-- ⑤ 行级操作：派发（排队）/ 中止（排队或执行中） -->
            <div
              v-if="t.state === 'pending' || t.state === 'running'"
              class="flex items-center gap-2 px-3 pb-2"
            >
              <button
                v-if="t.state === 'pending'"
                class="px-2.5 py-1 rounded-sm text-[10.5px] border border-primary/30 bg-primary/12 text-primary
                       active:scale-95 disabled:opacity-40 transition-transform"
                :disabled="dispatching"
                @click="runOne(t)"
              >▶ 派发这条</button>
              <button
                class="px-2.5 py-1 rounded-sm text-[10.5px] border border-danger/30 bg-danger-soft text-danger
                       active:scale-95 disabled:opacity-40 transition-transform"
                :disabled="abortBusy"
                @click="askAbort(t)"
              >■ 中止</button>
            </div>
          </div>
        </li>
      </ol>

      <!-- 渐进渲染：还有未挂载的行时给一个明确的「继续」入口 -->
      <button
        v-if="restCount(sec) > 0"
        class="w-full py-1.5 rounded-md text-[10.5px] text-content-sec bg-surface-card-2 active:bg-surface-card-2/70 transition-transform"
        @click="showMore(sec.key)"
      >还有 {{ restCount(sec) }} 条 · 点击展开</button>
    </section>

    <!-- ── 空态 ── -->
    <div v-if="!s.loading && !s.items.length && !s.err" class="glass rounded-md px-4 py-8 text-center">
      <div class="text-[12px] text-content-sec">当前没有任务记录</div>
      <div class="text-[10.5px] text-content-sec/60 mt-1.5 leading-relaxed">
        在「对话」页说一句「生成 10 张混沌帝国的卡」可登记任务
      </div>
    </div>
    <div v-else-if="noneAfterFilter" class="glass rounded-md px-4 py-6 text-center">
      <div class="text-[11.5px] text-content-sec">该状态下暂无任务</div>
      <div class="text-[10.5px] text-content-sec/60 mt-1">点上方「全部」看完整列表</div>
    </div>
    <div v-if="s.loading && !s.items.length" class="text-center text-[11px] text-content-sec py-6">加载中…</div>

    <!-- ══════ 任务详情悬浮半屏卡片（原有）══════ -->
    <Teleport to="body">
      <Transition name="kk-card">
        <div v-if="openId" class="fixed inset-0 z-[55]">
          <div class="kk-mask absolute inset-0 bg-black/45" @click="closeTask" />
          <div class="kk-sheet absolute left-2 right-2 bottom-2 max-h-[74vh]
                      glass-strong rounded-2xl overflow-hidden flex flex-col
                      shadow-[0_18px_60px_rgba(0,0,0,.55)] ring-1 ring-white/10">
            <!-- 头部：状态点 + 名称（不显示内部 id） -->
            <div class="shrink-0 flex items-center gap-2 px-3.5 py-2.5 border-b border-line">
              <span v-if="openTask" class="shrink-0 inline-flex items-center gap-1 text-[10px] font-medium" :class="stOf(openTask).text">
                <span class="w-1.5 h-1.5 rounded-full" :class="stOf(openTask).dot" />
                {{ stOf(openTask).label }}
              </span>
              <span v-if="openTask" class="shrink-0 text-[10px] px-1.5 py-0.5 rounded-sm bg-surface-card-2 text-content-sec">
                {{ kindName(openTask.kind) }}
              </span>
              <span class="flex-1 min-w-0 text-[11.5px] text-content truncate">
                {{ titleOf(openTask) }}
              </span>
              <button
                v-if="openTask && (openTask.state === 'pending' || openTask.state === 'running')"
                class="shrink-0 px-2 py-1 rounded-sm text-[10.5px] border border-danger/30 bg-danger-soft text-danger
                       active:scale-95 transition-transform"
                :disabled="abortBusy"
                @click="askAbort(openTask)"
              >■ 中止</button>
              <button
                class="shrink-0 w-7 h-7 rounded-lg bg-surface-card-2 text-content-sec flex items-center
                       justify-center text-[15px] active:scale-95 transition-transform"
                @click="closeTask"
                aria-label="关闭"
              >✕</button>
            </div>
            <!-- 详情正文 -->
            <div class="flex-1 min-h-0 overflow-y-auto px-3.5 py-3 space-y-2">
              <div v-if="detail[openId]?.loading" class="text-[11.5px] text-content-sec">读取中…</div>
              <div v-else-if="detail[openId]?.err" class="text-[11.5px] text-danger break-all">{{ detail[openId].err }}</div>
              <template v-else>
                <div class="text-[10px] text-content-sec/60 leading-relaxed flex flex-wrap gap-x-2">
                  <span>{{ openTask ? fmtTime(openTask.created_ts) : '' }}</span>
                  <span v-if="openTask?.civ">· {{ stripId(openTask.civ) }}</span>
                  <span v-if="openTask?.volume">· {{ stripId(openTask.volume) }}</span>
                  <span v-if="openTask?.chapter">· 第 {{ openTask.chapter }} 章</span>
                  <span v-if="openTask?.real">· 真实外呼</span>
                  <span v-if="totalN(openTask)">· 进度 {{ doneN(openTask) }}/{{ totalN(openTask) }}
                    <span v-if="failedN(openTask)" class="text-danger">（失败 {{ failedN(openTask) }}）</span>
                  </span>
                  <span v-if="openTask?.current">· {{ stripId(openTask.current) }}</span>
                </div>

                <!-- 卡名清单（详情里也保留） -->
                <div v-if="openTask?.card_groups?.length" class="space-y-0.5">
                  <div v-for="(g, gi) in openTask.card_groups" :key="gi" class="text-[10.5px] leading-relaxed">
                    <span class="text-primary/80">{{ stripId(g.kind_cn) }}</span>
                    <template v-if="groupItems(g).length">
                      <span class="text-content-sec"> · </span>
                      <span v-for="(nm, ni) in groupItems(g)" :key="ni" class="text-content/85">{{ nm }}<span v-if="ni < groupItems(g).length - 1" class="text-content-sec/50">、</span></span>
                    </template>
                    <span v-else class="text-content-sec/60">（{{ g.count }} 张，卡名未指定）</span>
                  </div>
                </div>

                <!-- 报错详情也用醒目框 -->
                <div v-if="openErrText" class="tb-err">
                  <span class="tb-err-bar" aria-hidden="true" />
                  <div class="tb-err-body">
                    <div class="tb-err-head"><span class="tb-err-title">✕ 错误详情</span></div>
                    <pre class="tb-err-text is-open">{{ openErrText }}</pre>
                  </div>
                </div>

                <pre class="text-[11px] text-content/88 leading-relaxed whitespace-pre-wrap break-words font-mono">{{ openResult }}</pre>
              </template>
            </div>
          </div>
        </div>
      </Transition>
    </Teleport>

    <!-- ══════ 解锁确认（fixed 居中：无论滚到哪都不会被漏看）══════ -->
    <Teleport to="body">
      <div
        v-if="confirming"
        class="fixed inset-0 z-[70] flex items-center justify-center px-5 bg-black/55 animate-fade-in"
        @click.self="confirming = false"
      >
        <div class="w-full max-w-[320px] rounded-md px-3.5 py-3 space-y-2.5
                    bg-surface-card border border-warning/40 shadow-3 animate-pop-in">
          <div class="flex items-center gap-1.5">
            <span class="text-[13px]">⚠️</span>
            <span class="text-[12px] font-medium text-warning">确认解锁真实 LLM</span>
          </div>
          <div class="text-[11px] text-content-sec leading-relaxed">
            解锁后<b class="text-content">真实 LLM 外呼</b>将被允许：会消耗额度、会写入资料库。
          </div>
          <div class="flex items-center gap-2 pt-0.5">
            <button
              class="flex-1 rounded-sm py-2 text-[11.5px] font-medium bg-warning-soft text-warning border
                     border-warning/35 active:scale-95 disabled:opacity-50 transition-transform"
              :disabled="lockBusy"
              @click="confirmUnlock"
            >{{ lockBusy ? '解锁中…' : '确认解锁' }}</button>
            <button
              class="flex-1 rounded-sm py-2 text-[11.5px] text-content-sec border border-line
                     active:scale-95 disabled:opacity-50 transition-transform"
              :disabled="lockBusy"
              @click="confirming = false"
            >取消</button>
          </div>
        </div>
      </div>
    </Teleport>

    <!-- ══════ 中止确认（新增；误杀执行器代价高，必须二次确认）══════ -->
    <Teleport to="body">
      <div
        v-if="abortTarget"
        class="fixed inset-0 z-[70] flex items-center justify-center px-5 bg-black/55 animate-fade-in"
        @click.self="abortBusy ? null : (abortTarget = null)"
      >
        <div class="w-full max-w-[320px] rounded-md px-3.5 py-3 space-y-2.5
                    bg-surface-card border border-danger/40 shadow-3 animate-pop-in">
          <div class="flex items-center gap-1.5">
            <span class="text-[13px]">■</span>
            <span class="text-[12px] font-medium text-danger">确认中止任务</span>
          </div>
          <div class="text-[11px] text-content-sec leading-relaxed">
            将请求后端中止<b class="text-content">「{{ titleOf(abortTarget) }}」</b>。
            已生成并落盘的内容不会被回滚，但未完成的部分会停下。
          </div>
          <div class="flex items-center gap-2 pt-0.5">
            <button
              class="flex-1 rounded-sm py-2 text-[11.5px] font-medium bg-danger-soft text-danger border
                     border-danger/35 active:scale-95 disabled:opacity-50 transition-transform"
              :disabled="abortBusy"
              @click="confirmAbort"
            >{{ abortBusy ? '中止中…' : '确认中止' }}</button>
            <button
              class="flex-1 rounded-sm py-2 text-[11.5px] text-content-sec border border-line
                     active:scale-95 disabled:opacity-50 transition-transform"
              :disabled="abortBusy"
              @click="abortTarget = null"
            >取消</button>
          </div>
        </div>
      </div>
    </Teleport>

    <div class="h-2" />
  </div>
</template>

<style>
/* ── 任务详情悬浮卡片：与大纲卡片共用同一条弹跳曲线，保证全局手感一致 ──
   只动 transform / opacity（DESIGN-v2 §3）。 */
.kk-card-enter-active .kk-mask,
.kk-card-leave-active .kk-mask {
  transition: opacity 220ms ease;
}
.kk-card-enter-active .kk-sheet {
  transition: transform 320ms cubic-bezier(.22, 1.4, .36, 1), opacity 200ms ease;
}
.kk-card-leave-active .kk-sheet {
  transition: transform 200ms cubic-bezier(.4, 0, 1, 1), opacity 170ms ease;
}
.kk-card-enter-from .kk-mask,
.kk-card-leave-to .kk-mask {
  opacity: 0;
}
.kk-card-enter-from .kk-sheet,
.kk-card-leave-to .kk-sheet {
  opacity: 0;
  transform: translate3d(0, 100%, 0) scale(0.98);
}
.kk-sheet {
  padding-bottom: env(safe-area-inset-bottom, 0px);
  will-change: transform;
}

/* ── SSE 实时日志：平滑入场，不整体重排（对标 Operit2 的增量渲染）── */
.tb-log-enter-active {
  transition: opacity 180ms ease-out, transform 180ms ease-out;
}
.tb-log-enter-from {
  opacity: 0;
  transform: translateY(-4px);
}
.tb-log-move {
  transition: transform 180ms ease-out;
}

/* ── 流式容器：overscroll-behavior: contain 防止滚到底把整页带着滚
   （Android WebView 嵌套滚动最容易踩的坑）。
   scroll-behavior 交给 JS 的 scrollTo({behavior:'smooth'})，避免两套平滑策略打架。 */
.scroll-follow {
  overscroll-behavior: contain;
  -webkit-overflow-scrolling: touch;
}

/* ══════════════════════════════════════════════════════════════
   时间轴（状态流）：左侧一条竖线 + 每个节点一颗状态色点。
   全部静态布局，无动画；色点颜色由 Tailwind 语义类给出（bg-primary 等）。
   ══════════════════════════════════════════════════════════════ */
.tb-timeline {
  position: relative;
  padding-left: 16px;
  margin: 0;
  list-style: none;
}
.tb-timeline::before {
  content: '';
  position: absolute;
  left: 5px;
  top: 6px;
  bottom: 6px;
  width: 1px;
  background: rgb(var(--divider-rgb) / 0.85);
}
.tb-node {
  position: relative;
  margin-bottom: 8px;
}
.tb-node:last-child {
  margin-bottom: 0;
}
.tb-dot {
  position: absolute;
  left: -15px;
  top: 15px;
  width: 11px;
  height: 11px;
  border-radius: 9999px;
  /* 静态描边环，把色点从竖线里「抬」出来；不是动画 */
  box-shadow: 0 0 0 3px rgb(var(--bg-rgb));
}

/* 折叠箭头：只转 transform */
.tb-arrow {
  display: inline-block;
  font-size: 14px;
  line-height: 1;
  transition: transform 200ms cubic-bezier(.22, 1, .36, 1);
}
.tb-arrow.is-open {
  transform: rotate(90deg);
}

/* 打字机光标：inline-block 竖条，只做 opacity 闪烁 */
.tb-caret {
  display: inline-block;
  width: 2px;
  height: 12px;
  margin-left: 2px;
  vertical-align: middle;
  background: rgb(var(--success-rgb));
  animation: tbCaretBlink 1s step-end infinite;
}
@keyframes tbCaretBlink {
  0%, 100% { opacity: 1; }
  50% { opacity: 0; }
}

/* 筛选条横向滚动不显示滚动条（避免底部多一条横杠） */
.tb-noscroll {
  scrollbar-width: none;
}
.tb-noscroll::-webkit-scrollbar {
  display: none;
}

/* ══════════════════════════════════════════════════════════════
   报错醒目框（DESIGN-v2 §5③ 硬性要求）
     bg-danger-soft + border-danger/40 + 左侧 3px 竖条
   长堆栈默认折叠（.is-clamped 用 -webkit-line-clamp 收成 3 行），
   展开后限高内滚 —— 只切换 class，不做 height/width 动画。
   ══════════════════════════════════════════════════════════════ */
.tb-err {
  display: flex;
  align-items: stretch;
  border-radius: var(--r-md);
  background: rgb(var(--danger-soft-rgb));
  border: 1px solid rgb(var(--danger-rgb) / 0.4);
  overflow: hidden;
}
.tb-err-bar {
  flex: none;
  width: 3px;
  background: rgb(var(--danger-rgb));
}
.tb-err-body {
  flex: 1 1 auto;
  min-width: 0;
  padding: 6px 9px;
}
.tb-err-head {
  display: flex;
  align-items: center;
  gap: 8px;
}
.tb-err-title {
  font-size: 10.5px;
  font-weight: 600;
  color: rgb(var(--danger-rgb));
}
.tb-err-toggle {
  margin-left: auto;
  flex: none;
  font-size: 9.5px;
  padding: 1px 6px;
  border-radius: var(--r-sm);
  border: 1px solid rgb(var(--danger-rgb) / 0.4);
  color: rgb(var(--danger-rgb));
  transition: transform 160ms ease;
}
.tb-err-toggle:active {
  transform: scale(0.94);
}
.tb-err-text {
  margin: 4px 0 0;
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  font-size: 10.5px;
  line-height: 1.6;
  white-space: pre-wrap;
  word-break: break-all;
  color: rgb(var(--danger-rgb) / 0.92);
}
.tb-err-text.is-clamped {
  display: -webkit-box;
  -webkit-line-clamp: 3;
  -webkit-box-orient: vertical;
  overflow: hidden;
}
.tb-err-text.is-open {
  max-height: 220px;
  overflow-y: auto;
  overscroll-behavior: contain;
}
</style>
