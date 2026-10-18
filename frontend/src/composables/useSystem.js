// useSystem.js —— 系统视图三子页的数据装配层
//
// ══════════════════════════════════════════════════════════
// 为什么单独一层（不写在组件里）
// ══════════════════════════════════════════════════════════
//
// 三个子页各自要拉数据，但有三条共性需求：
//   ① 结果要**缓存在 store.systemData**，切子页回来不重新拉
//   ② 并发保护：同一接口不能因为连点两次就发两次请求
//   ③ 错误要落到 state 上给界面显示，而不是 throw 到组件里白屏
//
// 写在组件里会导致三份重复逻辑 + 缓存失效。所以统一放这里。
//
// ── 真实接口（全部实测过，不是猜的）────────────────────────
// GET /api/tasks/list                  → {count, tasks:[{id,kind,state,created_ts,result_preview}]}
// GET /api/tasks/active                → {pending,running,totals,executor_alive,items}
// GET /api/tasks/{id}                  → 单任务详情
// GET /api/tasks/failures              → 失败列表
// GET /api/pipeline/funnel             → {progress, stages:[{id,name,status,done,file,dir}]}
// GET /api/pipeline/overview           → {funnel, chapters:{progress,next_chapter,...}}
// GET /api/layers/list                 → {canonical:[{layer,meaning,present,count}], actual, total}
// GET /api/layers/{L}/items?limit=     → {count, items:[{layer,key,title,chars,source,tags}]}
// GET /api/layers/{L}/item/{key}       → {content, chars}
// GET /api/library/index?layer=L4      → {count, items:[{key,title,category,chars,dirty,tags,note_status}]}
// GET /api/library/categories          → {categories:[{id,name,prefix,members}]}

import { systemData } from '../store.js'
import { api, LayerAPI, StatsAPI } from '../api.js'

// ── 并发去重：同一 key 的请求在飞行中时，复用同一个 Promise ──
const inflight = new Map()
// force 刷新的序号：让每次强制刷新都拿到独立 dedupe key，不复用飞行中的旧 Promise
let libSeq = 0
function dedupe(key, fn) {
  if (inflight.has(key)) return inflight.get(key)
  const p = Promise.resolve()
    .then(fn)
    .finally(() => inflight.delete(key))
  inflight.set(key, p)
  return p
}

/** 统一错误文案：把后端 {error} 也认出来 */
function errText(e) {
  if (!e) return '未知错误'
  if (typeof e === 'string') return e
  return e.message || String(e)
}

// ══════════════════════════════════════════════ ① 任务看板

/**
 * 拉任务列表（含活跃摘要）。
 *
 * 为什么两个接口一起拉：
 *   列表给"谁在排队/谁完成了"，摘要给"执行器活着吗/现在跑几个"。
 *   分开拉会导致数字和列表短暂不一致（用户看到"运行中 1"但列表里没有）。
 */
export async function loadTaskBoard(force = false) {
  const s = systemData.taskList
  if (s.loading) return
  if (!force && s.items.length && !s.err) return // 有缓存就用缓存
  s.loading = true
  s.err = ''
  try {
    const [list, act] = await Promise.all([
      dedupe('tasks:list', () => api.get('/api/tasks/list?limit=300')).catch(
        (e) => ({ error: errText(e) })
      ),
      dedupe('tasks:active', () => api.get('/api/tasks/active')).catch(() => ({})),
    ])
    if (list && list.error && !list.tasks) {
      s.err = list.error
    } else {
      // 后端字段：tasks[] ；有的版本叫 items[]
      s.items = list.tasks || list.items || []
      s.count = list.count || s.items.length
    }
    s.active = {
      pending: act.pending ?? 0,
      running: act.running ?? 0,
      // 【2026-10-18 修 · 数字口径错配】看板那张卡的标题是「失败」（原写「阻塞」），
      // 但这里只取 totals.blocked（实测=1），把 failed(16) + rejected(4) 全漏了，
      // 于是顶部显示 1、下方列表却有 21 条，用户一眼看出对不上。
      // 三者之和才等于「阻塞 / 失败」分组的真实条数。
      blocked:
        (act.totals?.blocked ?? 0) +
        (act.totals?.failed ?? 0) +
        (act.totals?.rejected ?? 0),
      done: act.totals?.done ?? 0,
      rejected: act.totals?.rejected ?? 0,
      executorAlive: !!act.executor_alive,
      executorPid: act.executor_pid ?? null,
      llmUnlocked: !!act.llm_unlocked,
    }
  } catch (e) {
    s.err = errText(e)
  } finally {
    s.loading = false
  }
}

/** 单任务详情（点开某一行时拉） */
export async function loadTaskDetail(id) {
  return dedupe(`task:${id}`, () => api.get(`/api/tasks/${encodeURIComponent(id)}`))
}

/** 按状态分组，供看板三列显示 */
export function groupTasks(items) {
  const g = { pending: [], running: [], done: [], blocked: [], other: [] }
  for (const t of items || []) {
    const st = String(t.state || '').toLowerCase()
    if (st === 'pending') g.pending.push(t)
    else if (st === 'running') g.running.push(t)
    else if (st === 'done') g.done.push(t)
    else if (st === 'blocked' || st === 'rejected' || st === 'failed') g.blocked.push(t)
    else g.other.push(t)
  }
  return g
}

// ══════════════════════════════════════════════ ② 大纲（L0-L3）

/** 大纲要展示的层（顺序即漏斗的逆向顺序，L0 在最后=最上游） */
export const OUTLINE_LAYERS = [
  { id: 'L0.5', name: '故事脊梁' },
  { id: 'L1x', name: '宏大对抗' },
  { id: 'L1', name: '具体总纲' },
  { id: 'L2', name: '分卷大纲' },
  { id: 'L3', name: '细纲' },
  { id: 'L0', name: '整本概括' },
]

/**
 * 拉大纲全层（每层一次请求，并发 6 个）。
 *
 * 顺便拉 funnel：它给出「哪层 done、哪层 pending」，是层头上的状态徽章来源。
 * 二者缺失其一不影响另一，所以用 allSettled 分别容错。
 */
export async function loadOutline(force = false) {
  const s = systemData.outline
  if (s.loading) return
  if (!force && s.levels.length && !s.err) return
  s.loading = true
  s.err = ''
  try {
    const futures = OUTLINE_LAYERS.map((L) =>
      dedupe(`layer:${L.id}`, () =>
        api.get(`/api/layers/${encodeURIComponent(L.id)}/items?limit=200`)
      ).catch((e) => ({ error: errText(e), __layer: L.id }))
    )
    const funnelP = dedupe('pipeline:funnel', () => api.get('/api/pipeline/funnel')).catch(
      () => null
    )
    const [results, funnel] = await Promise.all([Promise.all(futures), funnelP])

    // funnel.stages → {层id: {status, done}}
    const stageMap = {}
    for (const st of funnel?.stages || []) {
      stageMap[String(st.id).toUpperCase()] = st
    }

    s.levels = OUTLINE_LAYERS.map((L, i) => {
      const r = results[i] || {}
      const items = r.items || []
      const stage = stageMap[L.id.toUpperCase()] || null
      return {
        id: L.id,
        name: L.name,
        items,
        count: r.count ?? items.length,
        err: r.error || '',
        status: stage?.status || '',
        done: !!stage?.done,
        file: stage?.file || '',
      }
    })
    s.funnelProgress = funnel?.progress || ''
    s.funnelDone = funnel?.done ?? 0
    // 默认展开前两层（最常见要看 L3 细纲）
    if (!Object.keys(s.expanded).length) {
      s.expanded = { L3: true, L2: true }
    }
  } catch (e) {
    s.err = errText(e)
  } finally {
    s.loading = false
  }
}

/** 展开/折叠某层 */
export function toggleOutlineLevel(id) {
  const e = systemData.outline.expanded
  e[id] = !e[id]
}

/** 拉某条大纲条目的正文（点开看内容时） */
export async function loadOutlineItem(layer, key) {
  return dedupe(`layeritem:${layer}:${key}`, () =>
    api.get(
      `/api/layers/${encodeURIComponent(layer)}/item/${encodeURIComponent(key)}`
    )
  )
}

// ══════════════════════════════════════════════ ③ 卡片（L4）

/** 拉分类树（用于筛选条） */
export async function loadCardCategories(force = false) {
  const s = systemData.cards
  if (!force && s.categories.length) return
  try {
    const r = await dedupe('lib:categories', () => api.get('/api/library/categories'))
    s.categories = (r.categories || []).filter((c) => !c.archived)
  } catch (e) {
    // 分类拉不到不阻塞卡片列表，只是筛选条空着
    s.categories = []
  }
}

/**
 * 拉 L4 卡片列表。
 *
 * 注意：后端 `/api/library/index` 在一次请求里返回**全部**匹配项（受 limit 限制），
 * 没有服务端分页参数 `offset` 的保证。所以这里**一次拉足够多**（默认 800），
 * 然后在**前端做分类过滤 + 分页**。这样切分类是零延迟的，
 * 比每次点分类都打一次网络好得多（L4 有 700 张，一次拉的体积可接受）。
 */
export async function loadCards(force = false) {
  const s = systemData.cards
  if (s.loading) {
    // [*] 关键：写操作（新建/编辑/删除）后调 loadCards(true) 时，如果首屏那次
    //     loadCards() 还在飞行，原实现直接 return —— 强制刷新被静默丢弃，
    //     列表停留在旧数据（新建的卡明明落库了却看不见，就是这条导致的）。
    //     现在改为登记「待补刷」，等当前请求结束后自动再拉一次。
    if (force) s.pendingReload = true
    return
  }
  if (!force && s.items.length && !s.err) return
  s.loading = true
  s.err = ''
  try {
    // force 时用带序号的 key，避免复用「飞行中的旧 Promise」拿到陈旧结果。
    const cacheKey = force ? `lib:L4#${++libSeq}` : 'lib:L4'
    const [r] = await Promise.all([
      dedupe(cacheKey, () => api.get('/api/library/index?layer=L4&limit=2000')),
      loadCardCategories(),
    ])
    if (r.error && !r.items) {
      s.err = r.error
    } else {
      s.items = r.items || []
      s.count = r.count ?? s.items.length
      // 后端会在截断时回 truncated=true；这里透传，UI 可提示「仅显示前 N 张」
      s.truncated = !!r.truncated
      s.matched = r.matched ?? s.count
    }
  } catch (e) {
    s.err = errText(e)
  } finally {
    s.loading = false
    if (s.pendingReload) {
      s.pendingReload = false
      loadCards(true)
    }
  }
}

/** 前端过滤（分类 + 关键词）——切筛选不走网络 */
export function filteredCards() {
  const s = systemData.cards
  const cat = s.filterCategory
  const kw = s.filterKey.trim().toLowerCase()
  return s.items.filter((c) => {
    if (cat && c.category !== cat) return false
    if (kw) {
      const hay = `${c.key} ${c.title} ${c.category}`.toLowerCase()
      if (!hay.includes(kw)) return false
    }
    return true
  })
}

/** 分页切片 */
export function pagedCards() {
  const s = systemData.cards
  const all = filteredCards()
  const start = s.page * s.pageSize
  return {
    total: all.length,
    slice: all.slice(start, start + s.pageSize),
    pageCount: Math.max(1, Math.ceil(all.length / s.pageSize)),
  }
}

/** 拉卡片详情（带缓存；已拉过就直接用） */
export async function loadCardDetail(key) {
  const s = systemData.cards
  s.openKey = key
  const cached = s.detail[key]
  if (cached && cached.content) return cached
  s.detail[key] = { loading: true, err: '', content: '', chars: 0 }
  try {
    const r = await dedupe(`card:${key}`, () =>
      api.get(`/api/layers/L4/item/${encodeURIComponent(key)}`)
    )
    if (r.error && !r.content) {
      s.detail[key] = { loading: false, err: r.error, content: '', chars: 0 }
    } else {
      s.detail[key] = {
        loading: false,
        err: '',
        content: r.content || '',
        chars: r.chars ?? (r.content || '').length,
      }
    }
  } catch (e) {
    s.detail[key] = { loading: false, err: errText(e), content: '', chars: 0 }
  }
  return s.detail[key]
}

/** 关掉详情 */
export function closeCardDetail() {
  systemData.cards.openKey = ''
}

// ══════════════════════════════════════════════ 派生：卡片分类计数
export function cardCategoryCounts() {
  const m = new Map()
  for (const c of systemData.cards.items) {
    const k = c.category || '未分类'
    m.set(k, (m.get(k) || 0) + 1)
  }
  return [...m.entries()].sort((a, b) => b[1] - a[1])
}

// ══════════════════════════════════════════════ ④ 卡片的增 / 改 / 删
//
// 三条硬约束（都来自后端实现，不是猜测）：
//  ① 新建必须同时有 key 与 content，且 (layer,key) 不能重复；后端会写前快照。
//  ② 编辑传 content 时会**整体覆写文件**（不是 diff），所以前端必须先拿到原文再改，
//    不能用"只传改动部分"的思路。
//  ③ 每次写成功后必须**让缓存失效并重拉**，否则列表显示旧数据（任务三的核心）。

/** 写操作后统一失效卡片缓存，保证列表立刻反映真相 */
export function invalidateCards() {
  const s = systemData.cards
  s.items = []
  s.count = 0
  s.err = ''
  s.truncated = false
  s.matched = 0
  // 注意：这里**不能**复位 s.loading（真实请求还在飞），
  // 但必须清 pendingReload，否则可能残留一次无意义的补刷。
  s.pendingReload = false
  // 详情缓存也要清：同一 key 的内容可能刚被改过
  s.detail = {}
}

/** 建卡。成功返回 {ok,key,layer,...}；失败返回 {ok:false,error} */
export async function createCard({ key, title, content, category }) {
  const s = systemData.cards
  const payload = { key: String(key).trim(), title: String(title || '').trim(), content }
  const r = await LayerAPI.create('L4', payload)
  if (r && r.ok === false) return { ok: false, error: r.error || '新建失败' }
  // 建完立刻重拉，让新卡出现在列表里（任务三：切回卡片视图要能看到）
  invalidateCards()
  await loadCards(true)
  // 如果后端没有 category 字段，这里只做前端提示，不硬塞
  return { ok: true, ...r, category: category || '' }
}

/** 编辑已有卡片正文/标题 */
export async function updateCard(key, { title, content }) {
  const payload = {}
  if (typeof content === 'string') payload.content = content
  if (typeof title === 'string') payload.title = title.trim()
  const r = await LayerAPI.update('L4', key, payload)
  if (r && r.ok === false) return { ok: false, error: r.error || '保存失败' }
  // 该卡详情缓存直接失效
  delete systemData.cards.detail[key]
  invalidateCards()
  await loadCards(true)
  return { ok: true, ...r }
}

/** 删卡（危险操作，调用方必须二次确认） */
export async function removeCard(key) {
  const r = await LayerAPI.remove('L4', key)
  if (r && r.ok === false) return { ok: false, error: r.error || '删除失败' }
  delete systemData.cards.detail[key]
  if (systemData.cards.openKey === key) systemData.cards.openKey = ''
  invalidateCards()
  await loadCards(true)
  return { ok: true, ...r }
}

// ══════════════════════════════════════════════ ⑤ 统计 / 体检
//
// 数据来源：
//   /api/pipeline/materials → 声明 vs 实有（缺卡清单、漂移数、分类计数）
//   /api/layers/list        → 分层条目数
//   /api/pipeline/funnel    → 漏斗进度
//   这三个都是只读，可放心并发拉。

export async function loadStats(force = false) {
  const s = systemData.stats
  if (s.loading) return
  if (!force && s.loaded) return
  s.loading = true
  s.err = ''
  try {
    const [mat, lay, fun] = await Promise.all([
      dedupe('stats:materials', () => StatsAPI.materials()).catch((e) => ({
        error: errText(e),
      })),
      dedupe('stats:layers', () => StatsAPI.layers()).catch(() => null),
      dedupe('stats:funnel', () => StatsAPI.funnel()).catch(() => null),
    ])
    if (mat && mat.error && !mat.groups) {
      s.err = mat.error
    } else {
      s.materials = mat || {}
    }
    s.layers = lay || {}
    s.funnel = fun || {}
    s.loaded = true
    s.ts = Date.now()
  } catch (e) {
    s.err = errText(e)
  } finally {
    s.loading = false
  }
}

/**
 * 跑一次系统自检（只查不改）。
 *
 * 注意：这是 POST 且后端会真跑检查，可能耗时数秒 ——
 * 所以单独维护 selfCheckState，不阻塞统计页其余部分的渲染。
 */
export async function runSelfCheck() {
  const s = systemData.stats
  if (s.checking) return
  s.checking = true
  s.checkErr = ''
  s.checkResult = null
  try {
    const r = await StatsAPI.selfCheck(false)
    if (r && r.ok === false) {
      s.checkErr = r.error || '自检失败'
    } else {
      s.checkResult = r
    }
  } catch (e) {
    s.checkErr = errText(e)
  } finally {
    s.checking = false
  }
}

/** 缺卡清单（扁平化，供列表显示） */
export function missingCards() {
  const d = systemData.stats.materials?.declared
  return Array.isArray(d?.missing) ? d.missing : []
}

/** 分类计数（从 materials.groups 取，按数量降序） */
export function groupCounts() {
  const g = systemData.stats.materials?.groups
  if (!Array.isArray(g)) return []
  return [...g].sort((a, b) => (b.count || 0) - (a.count || 0))
}