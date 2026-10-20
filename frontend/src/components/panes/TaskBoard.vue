<script setup>
// TaskBoard.vue —— 任务看板（原 iframe 里的「任务看板」用 Vue3 重写）
//
// 设计要点：
//   · 顶部四个数字卡：排队 / 运行 / 阻塞 / 已完成。数字来自 /api/tasks/active
//   · 下面按状态分组列表：执行中 → 排队 → 已完成 → 阻塞
//   · 每行可点开看 result_preview（后端已经给预览，不必额外请求）
//   · 状态色：running=蓝（呼吸） pending=黄 done=绿 blocked=红
//   · 空态要有明确文案（不是一片空白）
//
// 2026-10-17 新增「▶ 执行待处理任务」：
//   ToolPkg 底部那两个全局按钮已删（与新底栏视觉冲突），功能迁到这里。
//   它调 /api/tasks/{id}/dispatch —— 后端会真 spawn 执行器，且内部有单飞锁。
//   没有 pending 任务时按钮禁用（避免空转）。
//
// 数据由 useSystem.js 装配，本组件只消费。缓存命中时不重复请求。
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import { systemData, pushLog, toast } from '../../store.js'
import {
  loadTaskBoard,
  groupTasks,
  loadTaskDetail,
} from '../../composables/useSystem.js'
import { useTasks } from '../../composables/useTasks.js'
const { refresh, dispatch, unlockLLM, lockLLM } = useTasks()
const s = systemData.taskList
const openId = ref('')
/** 当前打开详情的那条任务本体（卡片头部要显示状态/类型/id） */
const openTask = ref(null)
const detail = ref({})
const busy = ref(false)
const dispatching = ref(false)

// ── 授权锁（2026-10-17 从顶部细条搬到本面板）──
//   铁则：解锁只能由人点、必须二次确认、流程不得自动 touch allow 文件。
//   这里只调后端已有的 unlockLLM / lockLLM（走 real_llm_lock 模块写审计）。
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
    toast('已解锁：真实 LLM 外呼已允许')
    await refresh()
    await loadTaskBoard(true)
  }
}

async function doLock() {
  lockBusy.value = true
  const ok = await lockLLM()
  lockBusy.value = false
  if (ok) {
    toast('已上锁：真实 LLM 外呼已禁止')
    await refresh()
    await loadTaskBoard(true)
  }
}

const groups = computed(() => groupTasks(s.items))
const act = computed(() => s.active || {})

/** 真正的排队任务（可被 dispatch 的） */
const pendingList = computed(() => groups.value.pending)
const canDispatch = computed(() => pendingList.value.length > 0 && !dispatching.value)

/** 按时间倒序（新的在上） */
function byTime(list) {
  return [...list].sort((a, b) => (b.created_ts || 0) - (a.created_ts || 0))
}
const running = computed(() => byTime(groups.value.running))
const pending = computed(() => byTime(groups.value.pending))
const done = computed(() => byTime(groups.value.done).slice(0, 40))
const blocked = computed(() => byTime(groups.value.blocked))

// ══════ 【任务三·长列表渐进渲染】══════
// 为什么不直接上完整虚拟列表：
//   · 任务卡片行高不固定（progress / card_groups 展开后高度变化），
//     真虚拟化要测量 + 滚动锚定，实现复杂度高、移动端易抖；
//   · 数据上限是受限的：后端 task_store::list() 已 `clamp(1,500)`，
//     done 组另 `slice(0,40)`，所以最坏 500 行而非"几千"。
// 方案：分段揭示（首屏只挂 STEP 行）+ v-memo（已渲染行零 diff）。
//   首屏 DOM ≤ 4×20 行，滚动到底再按需追加 —— 实际手感等同虚拟列表，
//   但没有高度测量的风险，也不会破坏现有滚动位置与展开状态。
const VISIBLE_STEP = 20
const shown = ref({
  running: VISIBLE_STEP,
  pending: VISIBLE_STEP,
  blocked: VISIBLE_STEP,
  done: VISIBLE_STEP,
})
/** 当前组应渲染的子列表（未超过阈值时原样返回，不做无谓 slice） */
function visList(sec) {
  const lim = shown.value[sec.key] ?? VISIBLE_STEP
  return sec.list.length > lim ? sec.list.slice(0, lim) : sec.list
}
/** 该组还有多少条没渲染 */
function restCount(sec) {
  const lim = shown.value[sec.key] ?? VISIBLE_STEP
  return Math.max(0, sec.list.length - lim)
}
function showMore(key) {
  shown.value[key] = (shown.value[key] ?? VISIBLE_STEP) + VISIBLE_STEP
}

/**
 * 行级 v-memo 依赖签名（全部取原始值，不用对象引用）。
 *
 * 为什么必须是原始值：`tasks.items = r.items` 每次轮询整体换成新对象，
 * 若签名里放 `t.progress` 这类对象，引用必变 → v-memo 永远失效，等于没加。
 * 反过来只放影响渲染的标量，内容没变的行就真的跳过 diff。
 *
 * `updated_ts` 是兜底：任何一次 DB 回写都会改它，保证不出现陈旧行。
 */
function memoKey(t) {
  return [
    t.id, t.state, t.summary, t.created_ts, t.updated_ts,
    t.result_preview, t.current,
    t.progress?.done, t.progress?.total, t.progress?.failed,
    t.card_groups?.length,
  ]
}

const STATE_STYLE = {
  pending: { label: '排队', cls: 'text-warn bg-warn/12 border-warn/25' },
  running: { label: '执行中', cls: 'text-acc bg-acc/12 border-acc/25' },
  done: { label: '完成', cls: 'text-ok bg-ok/12 border-ok/25' },
  blocked: { label: '阻塞', cls: 'text-bad bg-bad/12 border-bad/25' },
  rejected: { label: '拒绝', cls: 'text-bad bg-bad/12 border-bad/25' },
  failed: { label: '失败', cls: 'text-bad bg-bad/12 border-bad/25' },
}
function st(state) {
  return STATE_STYLE[String(state || '').toLowerCase()] || {
    label: state || '未知',
    cls: 'text-fg-dim bg-ink-600/40 border-ink-600',
  }
}

/**
 * 【2026-10-18 修 · 信息失真】把"部分成功"从"失败"里区分出来。
 *
 * 为什么必须改：任务表顶层 state 只有 failed/done 两种粗粒度，而 cardgen
 * 一次派发多张卡时，`result` 里带的是 {total, success, failed}。实测 16 条
 * failed 里有 13 条其实是部分成功（合计已产出 44 张卡），但看板一律标红
 * 「失败」，用户完全看不出"这批其实成了大半"——这是比报错更糟的失真。
 *
 * 判定口径：
 *   · 只对 failed 生效（done/rejected/blocked/running 原样返回）
 *   · success > 0 才算部分完成；success=0 是真失败，不能美化
 *   · 数据源优先用 progress（后端已算好），退回 result_preview 里数
 */
function stOf(t) {
  const raw = String((t && t.state) || '').toLowerCase()
  const base = st(t.state)
  if (raw !== 'failed') return base

  let success = 0
  let total = 0
  const pg = t.progress || {}
  if (typeof pg.done === 'number' || typeof pg.total === 'number') {
    const failed = Number(pg.failed) || 0
    total = Number(pg.total) || 0
    success = Math.max(0, (Number(pg.done) || 0) - failed)
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
    return {
      label: '部分完成',
      cls: 'text-warn bg-warn/12 border-warn/25',
      success,
      total,
    }
  }
  return base
}

/** 修一下后端时间戳单位（秒 vs 毫秒） */
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
  return KIND_LABEL[k] || k || '任务'
}

/** 一行摘要：优先用 result_preview 的第一段 */
function brief(t) {
  const rp = (t.result_preview || '').trim()
  if (!rp) return ''
  // 去掉花括号噪音，取最像人话的一段
  const m = rp.match(/"(\w+)"\s*:\s*"([^"]{2,60})"/g)
  if (m && m.length) {
    return m
      .slice(0, 3)
      .map((x) => x.replace(/"/g, '').replace(/:/, '='))
      .join(' · ')
  }
  return rp.slice(0, 90)
}

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
    detail.value[t.id] = { loading: false, err: String(e.message || e) }
  }
}

/** 关闭任务详情悬浮卡片 */
function closeTask() {
  openId.value = ''
  openTask.value = null
}

async function reload() {
  busy.value = true
  await Promise.all([loadTaskBoard(true), refresh()])
  busy.value = false
  pushLog('任务看板已刷新', 'task')
  toast('已刷新')
}

/**
 * 执行待处理任务（从 ToolPkg 底部按钮迁移过来的功能）。
 *
 * 为什么只派发**一个** id：
 *   后端 dispatch 会 spawn 一个执行器进程，它自己按 limit 扫 pending。
 *   这里取最早的一条做入口即可 —— 执行器会继续按队列消费，
 *   不需要前端对每条 pending 各调一次（那会并发起多个执行器，撞单飞锁）。
 */
async function runPending() {
  const first = pending.value[0] || pendingList.value[0]
  if (!first) {
    toast('没有待处理任务', true)
    return
  }
  dispatching.value = true
  try {
    // 一次把「当前所有 pending」都交给执行器跑完，而不是只派 1 条。
    // 后端 dispatch 会 spawn 一个执行器进程，它自己按这个 limit 顺序消费队列；
    // 不传队列长度的话，19 条任务就要点 19 次，谈不上「一键」。
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

/**
 * ══════ 实时通道：SSE（Server-Sent Events）══════
 *
 * 2026-10-17 重构：彻底废弃 2 秒轮询。
 *
 * ## 为什么废弃轮询
 * 轮询的根本问题不是“2 秒慢”，而是 **cardgen 根本没有更细的进度可轮询**：
 * 它只有“卡开始”和“卡结束”两个时间点，中间 60~150 秒是空的。
 * 哪怕把轮询改成 100ms，看到的还是空——因为库里没有新数据。
 *
 * ## 新架构（对标 Operit2）
 *   cardgen.py print 一行
 *     → exec_pending.py Popen 逐行读 stdout
 *     → POST /api/tasks/{id}/event
 *     → Rust task_events::publish 广播
 *     → SSE 帧推到浏览器
 *   端到端实测 40.9ms。
 *
 * ## 断线重连与兵底
 * EventSource 自带重连（浏览器默认 3s 重试），但重连期间丢的事件要补：
 * Rust 侧 task_events 保留最近 200 条环形缓冲，新连接会立刻收到历史。
 * 另外 webui_task.result 字段继续写（兵底），页面刷新后 loadTaskBoard() 能拉回。
 */

const sseConnected = ref(false)
const logBox = ref(null)
const sseLogs = ref([])        // 实时日志（最多保留 300 条，防内存膨胀）
const sseError = ref('')      // 重连提示
let es = null
let reconnectTimer = null
const MAX_LOGS = 300

// ══════════════════════════════════════════════════════════════════
// 【2026-10-17 全流式】思考过程 + 正文双通道
//
// 三个通道语义必须分清，否则日志会很乱：
//   · sseLogs  → 程序日志（"⚠ 模型 502，正在重试" / "✅ CIVI-99-919 已落盘"）
//   · thinking → 模型自己的思考（"用户要的是一张文明主卡…先梳理谱系…"）
//   · content  → 模型正在写的正文（逐字打字机）
//
// ## 为什么用节流缓冲而不是直接 push
// 实测 longcat 一张卡 150s 吐数百~数千个 chunk，每 chunk 触发 Vue 响应式
// 更新会把主线程打死（卡片只有 60fps，一帧 16ms，1000 次更新排下来必然掉帧）。
// 做法：chunk 先拼进非响应式 buffer，50ms 的 timer 统一 flush 一次到 ref。
// 这样 1000 次更新压成 20 次，肉眼依旧丝滑（50ms ≈ 20fps，人眼感知连续）。
// ══════════════════════════════════════════════════════════════════
const thinkingText = ref('')      // 🧠 当前卡的完整思考（响应式，节流更新）
const contentText = ref('')       // ✍️ 当前卡的完整正文（响应式，节流更新）
const streamCard = ref(null)      // { code, name, category, idx, total }
const thinkOpen = ref(true)       // 【2026-10-18 改】思考区默认展开，常驻可读
const streamOpen = ref(true)      // 正文区是否展开
const modelNow = ref('')          // 当前模型名（状态指示器用）
const thinkDone = ref(false)      // 思考是否已结束（用于"完成"标记与自动折叠）

// 【2026-10-17 修】模板 ref 必须用 ref() 声明，不能用裸 let。
// 之前写成 `let thinkBox = null` + 模板 `ref="thinkBox"`，Vue 会把 ref 写进
// setup 返回的代理对象，但裸 let 变量不被代理，永远保持 null →
// 自动滚动代码 `if (thinkBox)` 永远为假，滚动从来没生效过。
// 用户反馈"思考8422字但看不到最新在想什么"就是这个 bug。
const thinkBox = ref(null)
const contentBox = ref(null)

// 非响应式缓冲（高频写入，避免触发渲染）
let bufThink = ''
let bufContent = ''
// 【注意】bufDirty 必须是 ref：模板里要用它驱动"模型思考中…"指示器。
// 如果做成普通对象，模板读到的永远是初始值，指示器不会亮。
const bufDirty = ref({ think: false, content: false })
// 【2026-10-18 新增】「思考中 / 正在输出」的**持续状态**标志。
//
// ## 为什么不能直接用 bufDirty 驱动指示器
// bufDirty 是**节流脏标记**，语义是"有未刷新的增量"，50ms 后就置回 false。
// 模型吐字是持续的，于是它每秒在 true/false 之间跳二十来次，
// 挂在它上面的 `v-if` 就带着"思考中…"整段文字疯狂显隐 —— 这就是用户看到的"闪烁"。
//
// ## 正确的分法
//   bufDirty → 管"这批增量刷没刷"（内部用）
//   thinkActive / contentActive → 管"这张卡此刻是不是还在吐字"（UI 用）
// 后者从 card_start 起为 true，到 card_done / card_fail 才转 false，
// 是稳定的布尔，`v-if` 挂它就不会抖。
const thinkActive = ref(false)
const contentActive = ref(false)
let flushTimer = null
const FLUSH_MS = 50
const MAX_THINK = 30000           // 思考上限 3 万字（超出截断，防内存膨胀）

/** 平滑滚到底部。
 *
 * 为什么不用 `el.scrollTop = el.scrollHeight`：
 * 每 50ms 一次硬跳会把滚动条砸得乱抖，用户往上翻历史时会被强行拽回。
 * 用 scrollTo({behavior:'smooth'}) 虽然也是每 50ms 调一次，
 * 但浏览器会把连续 smooth 请求合并成动画，观感是"持续跟随"而不是"抽搐"。
 */
function autoScroll(el) {
  if (!el) return
  try {
    el.scrollTo({ top: el.scrollHeight, behavior: 'smooth' })
  } catch (e) {
    el.scrollTop = el.scrollHeight   // 老 WebView 不支持 options 时降级
  }
}

function scheduleFlush() {
  if (flushTimer) return
  flushTimer = setTimeout(() => {
    flushTimer = null
    if (bufDirty.value.think) {
      bufDirty.value.think = false
      thinkingText.value = bufThink.slice(-MAX_THINK)
      // 只在展开时才滚：收起状态下容器 display:none，scrollHeight 为 0，
      // 滚了也没意义，还白花一次布局计算。
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
  thinkActive.value = false        // 新卡从"未开始吐字"起步
  contentActive.value = false
  thinkingText.value = ''
  contentText.value = ''
  thinkOpen.value = true           // 【2026-10-18 改】默认展开，新卡直接可读
  thinkDone.value = false
  streamCard.value = card || null
}

/** 收尾：把缓冲区剩余内容强制刷进 ref（卡结束时调用） */
function flushNow() {
  if (flushTimer) { clearTimeout(flushTimer); flushTimer = null }
  bufDirty.value = { think: false, content: false }
  thinkingText.value = bufThink.slice(-MAX_THINK)
  contentText.value = bufContent
}

// ══════════════════════════════════════════════════════════════════
// 【2026-10-17 修·空态常驻】面板显示开关
//
// ## 用户报的真实问题
// 重启 App 后打开任务看板，看到的是"任务卡在 0/10，实时日志和思考过程
// 两个框完全消失了，整个界面像死水"。
//
// ## 根因（不是 DOM 被删）
// 两个框的条件是纯数据驱动：
//   <div v-if="sseLogs.length || sseError">        实时日志
//   <div v-if="thinkingText || streamCard">        思考过程
// 而重启后 sseLogs 是空的（SSE 环形缓冲只保留最近 200 条，早滚出去了），
// streamCard 是 null（执行器已死，没有 card_start 事件进来）。
// → 两个框都判定为"没数据"而隐藏。
//
// ## 为什么这是真缺陷而不是小瑕疵
// 用户看到"任务在跑 但什么反馈都没有"，第一反应是"面板坏了/卡死了"。
// 但实际可能是：执行器进程被 App 重启杀掉、网关超时、锁被释放。
// **空态必须显性告知"为什么空"**，否则用户无法判断该等还是该动手。
//
// ## 改法
// 只要有任务处于 running，两个框就常驻显示，空数据时给明确空态文案：
//   日志框：「等待事件…（若长时间无输出，执行器可能已中断）」
//   思考框：「该任务尚未产生思考输出」
// 这样用户永远知道系统当前是"在等"还是"真死了"。
// ══════════════════════════════════════════════════════════════════
const hasRunning = computed(() => {
  const g = groups.value.running || []
  return Array.isArray(g) && g.length > 0
})
/** 日志框是否显示：有日志 / 有错误 / 有任务在跑（空态常驻） */
const showLogBox = computed(() => sseLogs.value.length > 0 || !!sseError.value || hasRunning.value)
/** 思考框是否显示：有思考 / 有卡片 / 有任务在跑（空态常驻） */
const showThinkBox = computed(() => !!thinkingText.value || !!streamCard.value || hasRunning.value)
/** 正文框是否显示：有正文 / 有任务在跑（空态常驻）
 *
 * 【2026-10-18 改】原来是 `!!contentText.value` —— 只有真有正文才显示，
 * 导致「正在等第一段正文」的整个思考阶段，界面上根本没有正文框，
 * 用户不知道这张卡后面会不会有正文输出。
 * 用户明确要求正文框固定常驻，所以跟思考框同口径：有正文 / 有卡片 / 有任务在跑。
 */
const showContentBox = computed(() => !!contentText.value || !!streamCard.value || hasRunning.value)

/** 追加一条日志（带上限，防止跑 80 张卡把内存吃满） */
function pushSseLog(entry) {
  const box = logBox.value
  // 只有用户已经在底部时才自动跟随；向上翻了就不抢滚动位置
  const atBottom = box ? (box.scrollHeight - box.scrollTop - box.clientHeight) < 60 : true
  sseLogs.value.push(entry)
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
    pushSseLog({ ts: ev.ts || Date.now(), line: d.line || '', stream: d.stream || 'stdout', task: d.task || '' })
    return
  }
  if (type === 'task_state') {
    // 同步更新 store 里的任务行（progress 字段用）
    const list = (s && s.tasks) || []
    const t = list.find(x => x.id === d.id)
    if (t) {
      t.state = d.state || t.state
      t.progress = {
        msg: d.msg || '', current: d.current || '', current_name: d.current_name || '',
        current_category: d.current_category || '', phase: d.phase || '',
        done: d.done || 0, total: d.total || 0, failed: d.failed || 0,
        results: d.results || [],
      }
      if (d.state === 'done' || d.state === 'failed' || d.state === 'rejected') {
        // 终态：拉一次全量（result 里有完整汇总）
        // 【2026-10-18 补】同时收掉「思考中 / 正在输出」的呼吸点。
        // 否则任务被 kill 或异常结束时，card_done/card_fail 事件可能永远不会来，
        // 呼吸点会一直闪，看起来像"卡还在跑"。
        thinkActive.value = false
        contentActive.value = false
        loadTaskBoard(true)
      }
    }
    return
  }
  if (type === 'card_start') {
    // 新卡开始 → 清空上一条的思考/正文，避免两卡的 token 混在一起
    resetStream({ code: d.cardId, name: d.name, category: d.category, idx: d.idx, total: d.total })
    pushSseLog({ ts: ev.ts || Date.now(), line: '▶ [' + (d.idx || '?') + '/' + (d.total || '?') + '] 开始生成 ' + (d.name || '') + '（' + (d.category || '') + '）', stream: 'card', task: d.task || '' })
    return
  }
  // ── 【2026-10-17 全流式】模型思考 token ──
  // 高频（每 chunk 一次），绝不能直接改 ref → 走 buffer + 50ms 节流
  if (type === 'thinking') {
    if (d.text) {
      bufThink += d.text
      bufDirty.value.think = true
      thinkActive.value = true      // 持续态：直到 card_done/fail 才灭
      scheduleFlush()
    }
    return
  }
  // ── 【2026-10-17 全流式】模型正文字 token（逐字打字机）──
  if (type === 'content') {
    if (d.text) {
      bufContent += d.text
      bufDirty.value.content = true
      contentActive.value = true    // 持续态：直到 card_done/fail 才灭
      scheduleFlush()
    }
    return
  }
  // ── 模型切换 / 失败 / 降级（状态指示器用）──
  if (type === 'model_start') {
    modelNow.value = d.model || ''
    pushSseLog({ ts: ev.ts || Date.now(), line: '🤖 模型 ' + (d.model || '') + '（第 ' + (d.attempt || 1) + ' 次尝试）', stream: 'card', task: d.task || '' })
    return
  }
  if (type === 'model_fail') {
    pushSseLog({ ts: ev.ts || Date.now(), line: '⚠ 模型 ' + (d.model || '') + ' 第 ' + (d.attempt || 1) + ' 次失败：' + (d.why || ''), stream: 'card', task: d.task || '' })
    return
  }
  if (type === 'model_fallback') {
    modelNow.value = d.to || ''
    pushSseLog({ ts: ev.ts || Date.now(), line: '↓ 降级：' + (d.frm || '?') + ' → ' + (d.to || '?'), stream: 'card', task: d.task || '' })
    return
  }
  if (type === 'card_done') {
    // 思考阶段彻底结束：标记"完成"，但【2026-10-18 改】不再自动收起。
    // 用户明确要求思考框常驻可读 —— 收起来等于把这一卡的分析过程藏掉。
    thinkDone.value = true
    bufDirty.value.think = false
    contentActive.value = false
    thinkActive.value = false
    flushNow()      // 把残留 token 刷完，否则末尾几个字看不到
    pushSseLog({ ts: ev.ts || Date.now(), line: '✅ [' + (d.idx || '?') + '/' + (d.total || '?') + '] ' + (d.name || '') + ' → ' + (d.chars || 0) + ' 字', stream: 'card', task: d.task || '' })
    return
  }
  if (type === 'card_fail') {
    thinkDone.value = true
    bufDirty.value.think = false
    contentActive.value = false
    thinkActive.value = false
    flushNow()
    pushSseLog({ ts: ev.ts || Date.now(), line: '❌ [' + (d.idx || '?') + '/' + (d.total || '?') + '] ' + (d.name || '') + ' 失败：' + (d.why || '') + ' ' + ((d.probs || []).join('; ')), stream: 'card', task: d.task || '' })
    return
  }
}

/** 建立 SSE 连接（含断线重连） */
function connectSse() {
  if (es) { try { es.close() } catch (e) {} es = null }
  try {
    es = new EventSource('/api/tasks/stream')
  } catch (e) {
    sseError.value = 'EventSource 不可用'
    return
  }
  es.onopen = () => { sseConnected.value = true; sseError.value = '' }
  es.onmessage = (m) => {
    try {
      const ev = JSON.parse(m.data)
      handleSseEvent(ev)
    } catch (e) { /* 忽略脏帧 */ }
  }
  es.onerror = () => {
    sseConnected.value = false
    sseError.value = '连接中断，3 秒后重连…'
    try { es.close() } catch (e) {}
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
  if (es) { try { es.close() } catch (e) {} es = null }
})
// 一按「执行」立刻重连一次，确保不漏掉开头几条
watch(() => dispatching.value, (v) => { if (!v) connectSse() })

watch(() => dispatching.value, (v) => { if (!v) schedulePoll() })
</script>

<template>
  <div class="h-full overflow-y-auto px-3 py-3 space-y-3">
    <!-- ── 顶部数字概览 ── -->
    <div class="grid grid-cols-4 gap-2">
      <div class="glass rounded-md px-2 py-2 text-center">
        <div class="text-[18px] font-semibold text-warn leading-none">
          {{ act.pending ?? 0 }}
        </div>
        <div class="text-[10px] text-fg-dim mt-1">排队</div>
      </div>
      <div class="glass rounded-md px-2 py-2 text-center">
        <div class="text-[18px] font-semibold text-acc leading-none flex items-center justify-center gap-1">
          <span
            v-if="(act.running ?? 0) > 0"
            class="inline-block w-1.5 h-1.5 rounded-full bg-acc animate-breathe"
          />
          {{ act.running ?? 0 }}
        </div>
        <div class="text-[10px] text-fg-dim mt-1">运行</div>
      </div>
      <div class="glass rounded-md px-2 py-2 text-center">
        <div class="text-[18px] font-semibold text-bad leading-none">
          {{ act.blocked ?? 0 }}
        </div>
        <div class="text-[10px] text-fg-dim mt-1">失败</div>
      </div>
      <div class="glass rounded-md px-2 py-2 text-center">
        <div class="text-[18px] font-semibold text-ok leading-none">
          {{ act.done ?? 0 }}
        </div>
        <div class="text-[10px] text-fg-dim mt-1">已完成</div>
      </div>
    </div>

    <!-- ── 执行器 + 锁 状态条 ── -->
    <div class="glass rounded-md px-3 py-2 space-y-2">
      <div class="flex items-center gap-3 text-[11px]">
        <span class="flex items-center gap-1.5">
          <span
            class="w-1.5 h-1.5 rounded-full"
            :class="act.executorAlive ? 'bg-ok' : 'bg-ink-500'"
          />
          <span :class="act.executorAlive ? 'text-ok' : 'text-fg-dim'">
            执行器{{ act.executorAlive ? '在线' : '空闲' }}
          </span>
          <span v-if="act.executorPid" class="font-mono text-fg-dim/60">
            #{{ act.executorPid }}
          </span>
        </span>
        <span class="flex items-center gap-1.5">
          <span
            class="w-1.5 h-1.5 rounded-full"
            :class="act.llmUnlocked ? 'bg-ok' : 'bg-bad'"
          />
          <span :class="act.llmUnlocked ? 'text-ok' : 'text-bad'">
            {{ act.llmUnlocked ? '已解锁' : '已上锁' }}
          </span>
        </span>
        <!-- 授权锁开关（2026-10-17 从顶部细条搬到这里）
             铁则：解锁必须二次确认，且只能由人点，流程不得自动 touch 文件。 -->
        <button
          class="ml-auto shrink-0 px-2 py-1 rounded-sm text-[10.5px] font-medium
                 border active:scale-95 disabled:opacity-40 ripple-host
                 transition-colors"
          :class="act.llmUnlocked
            ? 'bg-bad/12 text-bad border-bad/30'
            : 'bg-ok/12 text-ok border-ok/30'"
          :disabled="lockBusy"
          :title="act.llmUnlocked ? '点击上锁，禁止真实外呼' : '点击解锁，允许真实外呼'"
          @click="act.llmUnlocked ? doLock() : askUnlock()"
        >
          {{ lockBusy ? '…' : (act.llmUnlocked ? '🔓 上锁' : '🔒 解锁') }}
        </button>
        <button
          class="shrink-0 text-fg-dim active:scale-95 disabled:opacity-40"
          :disabled="busy"
          @click="reload"
        >
          {{ busy ? '刷新中…' : '刷新' }}
        </button>
      </div>

      <!-- ── 实时日志（SSE）────────── -->
      <div v-if="showLogBox" class="rounded-sm border border-line/60 overflow-hidden">
        <div class="flex items-center gap-2 px-2.5 py-1.5 bg-ink-700/40">
          <span class="w-1.5 h-1.5 rounded-full shrink-0" :class="sseConnected ? 'bg-ok animate-pulse' : 'bg-bad'" />
          <span class="text-[10.5px] text-fg-dim">实时日志</span>
          <span class="text-[9.5px] text-fg-dim/60 font-mono">{{ sseLogs.length }}</span>
          <span v-if="sseError" class="text-[9.5px] text-warn ml-auto">{{ sseError }}</span>
          <button v-else-if="sseLogs.length" class="text-[9.5px] text-fg-dim/70 ml-auto" @click="sseLogs = []">清空</button>
          <span v-else class="text-[9px] text-fg-dim/50 ml-auto">{{ sseConnected ? '已连接，等待事件…' : '未连接' }}</span>
        </div>
        <div ref="logBox" class="max-h-56 overflow-y-auto px-2.5 py-1.5 space-y-0.5 bg-ink-900/40 scroll-follow">
          <TransitionGroup name="sse-log">
            <div
              v-for="(l, i) in sseLogs"
              :key="i"
              class="text-[10px] leading-relaxed font-mono whitespace-pre-wrap break-all"
              :class="{ 'text-ok/90': l.line.includes('✅'), 'text-warn/90': l.line.includes('⚠'), 'text-bad/90': l.line.includes('❌'), 'text-fg-dim': !l.line.includes('✅') && !l.line.includes('⚠') && !l.line.includes('❌') }"
            >{{ l.line }}</div>
          </TransitionGroup>
          <!-- 【空态】有任务在跑但没有任何日志 —— 必须明说，否则用户以为界面死了 -->
          <div
            v-if="!sseLogs.length"
            class="text-[10px] text-fg-dim/55 italic leading-relaxed"
          >
            暂无日志事件。<br>
            有任务在运行却没输出，通常是执行器进程已中断（App 重启会杀掉它）。
            点上面「▶ 执行待处理任务」可重新拉起。
          </div>
        </div>
      </div>
      <!-- ── 【2026-10-17 全流式】模型思考过程（可折叠）──
           默认收起：思考动辄几千字，展开会把正文挤到看不见。
           但它必须"存在且可展开"，因为判断模型有没有想偏，
           唯一的依据就是看它思考了什么。 -->
      <div
        v-if="showThinkBox"
        class="rounded-sm border border-line/60 overflow-hidden"
      >
        <button
          class="w-full flex items-center gap-2 px-2.5 py-1.5 bg-ink-700/40 text-left"
          @click="thinkOpen = !thinkOpen"
        >
          <!-- 【2026-10-18 改】指示器从「🧠 闪烁表情 + 闪烁的『思考中…』文字」
               换成「固定图标 + 一颗呼吸圆点」。
               原写法把 animate-pulse 挂在 emoji 和文字上，视觉上是整块在闪，很吵；
               现在只让一颗 6px 小圆点呼吸，状态仍然一眼可见，但不再晃眼。
               圆点只在 thinkActive（真的还在吐字）时出现，不是靠 bufDirty 抖。 -->
          <span class="text-[11px] shrink-0">🧠</span>
          <span class="text-[10.5px] text-fg-dim shrink-0">思考过程</span>
          <span
            v-if="thinkActive && !thinkDone"
            class="w-1.5 h-1.5 rounded-full bg-acc shrink-0 animate-dotbreathe"
            title="模型正在思考"
          />
          <span
            v-if="thinkDone"
            class="text-[9px] text-ok/70 shrink-0"
          >完成</span>
          <span class="text-[9.5px] text-fg-dim/60 font-mono shrink-0">
            {{ thinkingText.length ? thinkingText.length + ' 字' : '' }}
          </span>
          <span class="ml-auto acc-arrow text-fg-dim/60 shrink-0" :class="{ 'is-open': thinkOpen }">›</span>
        </button>
        <div
          v-show="thinkOpen"
          ref="thinkBox"
          class="max-h-56 overflow-y-auto px-2.5 py-1.5 bg-ink-900/50 scroll-follow"
        >
          <div
            v-if="!thinkingText"
            class="text-[10px] text-fg-dim/50 italic"
          >{{ hasRunning ? '有任务在运行，但当前卡片尚未产生思考输出（执行器可能已中断）' : '（该模型不输出思考过程，或尚未开始）' }}</div>
          <div
            v-else
            class="text-[10px] leading-relaxed font-mono whitespace-pre-wrap break-all text-fg-dim/80"
          >{{ thinkingText }}</div>
          <div
            v-if="thinkingText.length >= MAX_THINK"
            class="mt-1 text-[9px] text-warn/70"
          >⚠ 思考过程过长，已折叠较早内容（仅保留最近 {{ MAX_THINK }} 字）</div>
        </div>
      </div>

      <!-- ── 【2026-10-17 全流式】正文实时输出（打字机）──
           这是判断"模型有没有把设定写偏"最直接的窗口：
           卡片正文在生成的那一刻就能看到，不用等落盘。 -->
      <div
        v-if="showContentBox"
        class="rounded-sm border border-line/60 overflow-hidden"
      >
        <button
          class="w-full flex items-center gap-2 px-2.5 py-1.5 bg-ink-700/40 text-left"
          @click="streamOpen = !streamOpen"
        >
          <!-- 【2026-10-18 改】同思考框：闪动的「正在输出…」文字 → 一颗呼吸圆点 -->
          <span class="text-[11px] shrink-0">✍️</span>
          <span class="text-[10.5px] text-fg-dim shrink-0">正文输出</span>
          <span
            v-if="contentActive"
            class="w-1.5 h-1.5 rounded-full bg-ok shrink-0 animate-dotbreathe"
            title="正在写入正文"
          />
          <span class="text-[9.5px] text-fg-dim/60 font-mono shrink-0">{{ contentText.length }} 字</span>
          <span class="ml-auto acc-arrow text-fg-dim/60 shrink-0" :class="{ 'is-open': streamOpen }">›</span>
        </button>
        <div
          v-show="streamOpen"
          ref="contentBox"
          class="max-h-56 overflow-y-auto px-2.5 py-1.5 bg-ink-900/50 scroll-follow"
        >
          <div
            v-if="!contentText"
            class="text-[10px] text-fg-dim/50 italic"
          >{{ hasRunning ? '有任务在运行，但当前卡片尚未开始写正文' : '（尚未产生正文输出）' }}</div>
          <span
            v-else
            class="text-[10.5px] leading-relaxed whitespace-pre-wrap break-words text-fg/85"
          >{{ contentText }}<span
            v-if="contentActive"
            class="inline-block w-1 h-3 bg-ok align-middle ml-0.5 animate-dotbreathe"
          /></span>
        </div>
      </div>

      <!-- 解锁确认（防止误触导致真跑烧钱烧时间）
           【2026-10-18 修·真 bug】原来是**内联**在本面板末尾的一个区块。
           实测证据：点顶部「🧠 解锁」按钮后，确认框确实被渲染了，
           但它的 getBoundingClientRect().top = 993px，
           而手机视口只有 739px —— 落在屏幕外，用户什么都没看见，
           于是报告「点解锁没有反应」。
           改为 fixed 居中的模态浮层：无论页面滚到哪、无论按钮在哪，
           确认框永远出现在视口正中，不可能被漏看。 -->
      <div
        v-if="confirming"
        class="fixed inset-0 z-[70] flex items-center justify-center px-5
               bg-black/55 backdrop-blur-[2px] animate-fade-in"
        @click.self="confirming = false"
      >
        <div
          class="w-full max-w-[320px] rounded-md px-3.5 py-3 space-y-2.5
                 bg-ink-800 border border-warn/40 shadow-2 animate-pop-in"
        >
          <div class="flex items-center gap-1.5">
            <span class="text-[13px]">⚠️</span>
            <span class="text-[12px] font-medium text-warn">确认解锁真实 LLM</span>
          </div>
          <div class="text-[11px] text-fg-dim leading-relaxed">
            解锁后<b class="text-fg">真实 LLM 外呼</b>将被允许：会消耗额度、会写入资料库。
          </div>
          <div class="flex items-center gap-2 pt-0.5">
            <button
              class="flex-1 rounded-sm py-2 text-[11.5px] font-medium
                     bg-warn/15 text-warn border border-warn/35 active:scale-95
                     disabled:opacity-50"
              :disabled="lockBusy"
              @click="confirmUnlock"
            >{{ lockBusy ? '解锁中…' : '确认解锁' }}</button>
            <button
              class="flex-1 rounded-sm py-2 text-[11.5px]
                     text-fg-dim border border-ink-600 active:scale-95
                     disabled:opacity-50"
              :disabled="lockBusy"
              @click="confirming = false"
            >取消</button>
          </div>
        </div>
      </div>

      <!-- 执行待处理任务（原 ToolPkg 全局按钮，迁到这里） -->
      <div class="flex items-center gap-2">
        <button
          class="flex-1 rounded-sm py-2 text-[12px] font-medium ripple-host
                 bg-acc/12 text-acc border border-acc/30
                 active:scale-[0.98] disabled:opacity-35"
          :class="canDispatch ? 'glow-btn' : ''"
          :disabled="!canDispatch"
          @click="runPending"
        >
          {{ dispatching ? '派发中…' : '▶ 执行待处理任务' }}
        </button>
        <span class="shrink-0 text-[10px] text-fg-dim/70 font-mono">
          {{ pending.length ? `${pending.length} 条排队` : '队列空' }}
        </span>
      </div>
      <div class="text-[10px] text-fg-dim/50 leading-relaxed">
        派发后由后端 spawn 执行器按队列消费；真跑需先在顶部解锁授权。
      </div>
    </div>

    <!-- ── 错误 ── -->
    <div
      v-if="s.err"
      class="rounded-md px-3 py-2 text-[11px] bg-bad/10 text-bad break-all"
    >{{ s.err }}</div>

    <!-- ── 分组列表 ── -->
    <template v-for="sec in [
      { key: 'running', title: '执行中', list: running, tone: 'acc' },
      { key: 'pending', title: '排队中', list: pending, tone: 'warn' },
      { key: 'blocked', title: '阻塞 / 失败', list: blocked, tone: 'bad' },
      { key: 'done', title: '最近完成', list: done, tone: 'ok' },
    ]" :key="sec.key">
      <div v-if="sec.list.length" class="space-y-1.5">
        <div class="flex items-center gap-2 px-1">
          <span
            class="w-1 h-3 rounded-full"
            :class="{
              'bg-acc': sec.tone === 'acc',
              'bg-warn': sec.tone === 'warn',
              'bg-bad': sec.tone === 'bad',
              'bg-ok': sec.tone === 'ok',
            }"
          />
          <span class="text-[11px] text-fg-dim">{{ sec.title }}</span>
          <span class="text-[10px] text-fg-dim/60 font-mono">{{ sec.list.length }}</span>
        </div>

        <div
          v-for="t in visList(sec)"
          :key="t.id"
          v-memo="memoKey(t)"
          class="glass card-cyber overflow-hidden"
        >
          <button
            class="w-full text-left px-3 py-2 flex items-center gap-2 active:bg-ink-700/40"
            @click="toggle(t)"
          >
            <span
              class="shrink-0 text-[10px] px-1.5 py-0.5 rounded-sm border font-mono"
              :class="stOf(t).cls"
            >{{ stOf(t).label }}</span>
            <!-- 主标题：人话（要建什么卡）。任务号降级到下方小字。
                 用户原话：「任务编号是给系统看的，请映射需要建立具体什么内容」 -->
            <span class="flex-1 min-w-0 text-[11.5px] text-fg truncate">
              {{ t.summary || kindName(t.kind) }}
            </span>
            <!-- 成败明细：部分成功时把「成了几张 / 坏了几张」直接标在行上，
                 不用点开就知道这批到底产出多少。只有部分完成才显示，
                 全成（done）不显示，避免噪声。 -->
            <span
              v-if="stOf(t).success > 0"
              class="shrink-0 text-[10px] font-mono text-ok/90"
            >✓{{ stOf(t).success }}<span
              v-if="stOf(t).total - stOf(t).success > 0"
              class="text-bad/80"
            > / ✗{{ stOf(t).total - stOf(t).success }}</span></span>
            <span class="shrink-0 text-[10px] text-fg-dim/70 font-mono">
              {{ fmtTime(t.created_ts) }}
            </span>
            <span class="shrink-0 text-[10px] text-fg-dim">
              {{ openId === t.id ? '▴' : '▾' }}
            </span>
          </button>

          <!-- 实时进度：执行中必须能看到「正在生成 XX」，否则用户以为没在跑。
               数据来源：/api/tasks/list 的 progress 字段（由 cardgen.py
               在每次调 LLM 之前 push_progress 写入）。 -->
          <div
            v-if="t.state === 'running' && t.progress && (t.progress.msg || t.progress.current_name)"
            class="px-3 pb-1.5 -mt-1 space-y-1"
          >
            <div class="flex items-center gap-1.5">
              <span class="w-1.5 h-1.5 rounded-full bg-acc animate-pulse shrink-0" />
              <span class="text-[10.5px] text-acc truncate">
                {{ t.progress.msg || ('正在生成 ' + t.progress.current_name) }}
              </span>
            </div>
            <div
              v-if="t.progress.total > 0"
              class="h-1 rounded-full bg-ink-600/60 overflow-hidden"
            >
              <div
                class="h-full bg-acc transition-all duration-500"
                :style="{ width: Math.min(100, Math.round((t.progress.done / t.progress.total) * 100)) + '%' }"
              />
            </div>
            <div v-if="t.progress.total > 0" class="flex items-center gap-2 text-[9.5px] text-fg-dim/80 font-mono">
              <span>{{ t.progress.done }} / {{ t.progress.total }}</span>
              <span v-if="t.progress.failed > 0" class="text-bad">失败 {{ t.progress.failed }}</span>
            </div>
          </div>
          <!-- 第二行：具体卡名清单（这才是用户要看的「建什么」） -->
          <div
            v-if="t.card_groups && t.card_groups.length && openId !== t.id"
            class="px-3 pb-2 -mt-0.5 space-y-0.5"
          >
            <div
              v-for="(g, gi) in t.card_groups"
              :key="gi"
              class="text-[10.5px] leading-relaxed"
            >
              <span class="text-acc/80">{{ g.kind_cn }}</span>
              <template v-if="g.items && g.items.length">
                <span class="text-fg-dim"> · </span>
                <span
                  v-for="(nm, ni) in g.items"
                  :key="ni"
                  class="text-fg/85"
                >{{ nm }}<span
                  v-if="ni < g.items.length - 1"
                  class="text-fg-dim/50"
                >、</span></span>
              </template>
              <span v-else class="text-fg-dim/60">
                （{{ g.count }} 张，卡名未指定）
              </span>
            </div>
            <!-- 任务号：给系统看的，缩到最小，不删（排障要用） -->
            <div class="text-[9.5px] text-fg-dim/40 font-mono truncate">
              {{ t.id }}
            </div>
          </div>

          <!-- 无卡名可显示时：退回原来的结果摘要 -->
          <div
            v-else-if="brief(t) && openId !== t.id"
            class="px-3 pb-2 -mt-0.5 text-[10.5px] text-fg-dim leading-relaxed line-clamp-2"
          >{{ brief(t) }}</div>
        </div>

        <!-- 【任务三】渐进渲染：还有未挂载的行时给一个明确的「继续」入口 -->
        <button
          v-if="restCount(sec) > 0"
          class="w-full py-1.5 rounded-md text-[10.5px] text-fg-dim
                 bg-ink-700/30 active:bg-ink-700/55 transition-colors"
          @click="showMore(sec.key)"
        >还有 {{ restCount(sec) }} 条 · 点击展开</button>
      </div>
    </template>
    <!-- ══════ 任务详情悬浮卡片（底部上滑半屏）══════
         用户要求：业务信息用悬浮卡片展示，不要行内展开把列表顶走。
         关掉后列表滚动位置原封不动。 -->
    <Teleport to="body">
      <Transition name="kk-card">
        <div v-if="openId" class="fixed inset-0 z-[55]">
          <div
            class="kk-mask absolute inset-0 bg-black/45 backdrop-blur-[2px]"
            @click="closeTask"
          />
          <div
            class="kk-sheet absolute left-2 right-2 bottom-2 max-h-[74vh]
                   glass-strong rounded-2xl overflow-hidden flex flex-col
                   shadow-[0_18px_60px_rgba(0,0,0,.55)] ring-1 ring-white/10"
          >
            <!-- 头部 -->
            <div class="shrink-0 flex items-center gap-2 px-3.5 py-2.5
                        border-b border-ink-600/60">
              <span
                v-if="openTask"
                class="shrink-0 text-[10px] px-1.5 py-0.5 rounded-sm border font-mono"
                :class="st(openTask.state).cls"
              >{{ st(openTask.state).label }}</span>
              <span
                v-if="openTask"
                class="shrink-0 text-[10px] px-1.5 py-0.5 rounded-sm
                       bg-ink-600/50 text-fg-dim"
              >{{ kindName(openTask.kind) }}</span>
              <span class="flex-1 min-w-0 text-[11.5px] text-fg truncate">
                {{ openTask?.summary || openId }}
              </span>
              <button
                class="shrink-0 w-7 h-7 rounded-lg bg-ink-700/70 text-fg-dim
                       flex items-center justify-center text-[15px] active:scale-95
                       transition-transform"
                @click="closeTask"
                aria-label="关闭"
              >✕</button>
            </div>
            <!-- 详情正文 -->
            <div class="flex-1 min-h-0 overflow-y-auto px-3.5 py-3 space-y-2">
              <div v-if="detail[openId]?.loading" class="text-[11.5px] text-fg-dim">
                读取中…
              </div>
              <div
                v-else-if="detail[openId]?.err"
                class="text-[11.5px] text-bad break-all"
              >{{ detail[openId].err }}</div>
              <template v-else>
                <div class="text-[10px] text-fg-dim/60 leading-relaxed flex flex-wrap gap-x-2">
                  <span>{{ openTask ? fmtTime(openTask.created_ts) : '' }}</span>
                  <span v-if="openTask?.civ">· {{ openTask.civ }}</span>
                  <span v-if="openTask?.volume">· {{ openTask.volume }}</span>
                  <span v-if="openTask?.chapter">· 第 {{ openTask.chapter }} 章</span>
                  <span v-if="openTask?.real">· 真实外呼</span>
                  <span v-if="openTask?.total">
                    · 进度 {{ openTask.done || 0 }}/{{ openTask.total }}
                    <span v-if="openTask.failed" class="text-bad">（失败 {{ openTask.failed }}）</span>
                  </span>
                  <span v-if="openTask?.current">· {{ openTask.current }}</span>
                </div>
                <pre
                  class="text-[11px] text-fg/88 leading-relaxed whitespace-pre-wrap
                         break-words font-mono"
                >{{ detail[openId]?.data?.result_preview || openTask?.result_preview || '（暂无输出）' }}</pre>
              </template>
            </div>
          </div>
        </div>
      </Transition>
    </Teleport>

    <!-- ── 空态 ── -->
    <div
      v-if="!s.loading && !s.items.length && !s.err"
      class="glass rounded-md px-4 py-8 text-center"
    >
      <div class="text-[12px] text-fg-dim">当前没有任务记录</div>
      <div class="text-[10.5px] text-fg-dim/60 mt-1.5 leading-relaxed">
        在「对话」页说一句「生成 10 张混沌帝国的卡」可登记任务
      </div>
    </div>

    <div
      v-if="s.loading && !s.items.length"
      class="text-center text-[11px] text-fg-dim py-6"
    >加载中…</div>

    <div class="h-2" />
  </div>
</template>

<style>
/* ── 任务详情悬浮卡片：与大纲卡片共用同一条弹跳曲线，保证全局手感一致 ── */
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
.sse-log-enter-active {
  transition: opacity 180ms ease-out, transform 180ms ease-out;
}
.sse-log-enter-from {
  opacity: 0;
  transform: translateY(-4px);
}
.sse-log-move {
  transition: transform 180ms ease-out;
}

/* ── 【2026-10-17】思考/正文流式容器 ──
   overscroll-behavior: contain 防止滚到底后把整个页面带着滚（Android WebView
   里嵌套滚动最容易踩的坑：用户想滚思考框，结果整页跳走了）。
   scroll-behavior 交给 JS 的 scrollTo({behavior:'smooth'}) 控制，
   不在这里写，避免两套平滑策略打架。 */
.scroll-follow {
  overscroll-behavior: contain;
  -webkit-overflow-scrolling: touch;
}
</style>