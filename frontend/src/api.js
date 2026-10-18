// api.js —— 所有后端通信的唯一出口
//
// 设计原则（重要）：
//   · 前端任何组件都**不直接 fetch**，全部走这里
//   · 将来从轮询换成 SSE/WebSocket，只改这一个文件
//
// `npm run dev` 时 Vite 的 proxy 会把 /api 转发到 8270；
// 生产环境（Rust 托管 dist）时是同源请求，不需要任何配置。

const BASE = '';

async function req(path, opt = {}) {
  const res = await fetch(BASE + path, {
    headers: { Accept: 'application/json', ...(opt.headers || {}) },
    ...opt,
  });
  // 后端在 400/404/409/500 时也返回 JSON，需要解析出来（错误原因要能显示给用户）
  if (!res.ok && [400, 404, 409, 500].includes(res.status)) {
    try {
      const j = await res.json();
      return { ok: false, error: j.error || j.message || `HTTP ${res.status}`, ...j };
    } catch {
      /* fallthrough */
    }
  }
  if (!res.ok) throw new Error(`HTTP ${res.status} ${path}`);
  return res.json();
}

export const api = {
  get: (p) => req(p),
  post: (p, body) =>
    req(p, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body ?? {}),
    }),
  put: (p, body) =>
    req(p, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body ?? {}),
    }),
  del: (p) => req(p, { method: 'DELETE' }),
};

// ── 业务接口（集中在这里，便于一眼看全）──
export const ChatAPI = {
  /** 发一句话给中枢，返回 {ok, reply, plan, task_ids} */
  //
  // 【session 默认值必须与后端一致】后端 `governance::DEFAULT_SESSION = "chat"`。
  // 曾经这里写 'default'、后端写 'chat'，两边不传参时各落一个会话，
  // 表现就是"刷新页面历史没了"（读的是另一个会话）。
  // 改这里要同步改 crates/ne-agent/src/governance/mod.rs。
  send: (message, session = 'chat') =>
    api.post('/api/chat', { message, session }),
  /** 历史（后端 GET /api/chat/history 已实现，返回最近 N 条） */
  history: (session = 'chat') =>
    api
      .get(`/api/chat/history?session=${encodeURIComponent(session)}`)
      .catch(() => ({ messages: [] })),
};

export const TaskAPI = {
  /** 活跃任务摘要（胶囊/悬浮窗轮询） */
  active: () => api.get('/api/tasks/active'),
  /** 任务列表 */
  list: () => api.get('/api/tasks/list'),
  /** 单任务详情 */
  detail: (id) => api.get(`/api/tasks/${encodeURIComponent(id)}`),
  /** 派发执行器 */
  dispatch: (id, limit = 1) =>
    api.post(`/api/tasks/${encodeURIComponent(id)}/dispatch`, { limit }),
  /** 取消 */
  cancel: (id) => api.del(`/api/tasks/${encodeURIComponent(id)}`),
};

export const LockAPI = {
  status: () => api.get('/api/system/llm-lock'),
  unlock: () => api.post('/api/system/llm-lock/unlock', {}),
  lock: () => api.post('/api/system/llm-lock/lock', {}),
};

export const SystemAPI = {
  status: () => api.get('/api/system/status'),
  gate: () => api.get('/api/system/gate'),
};

// ══════════════════════════════════════════════════════════
// 模型配置 —— 设置中心用
//
// 后端三处同步：OMP 池（apiKeys 结构）、data（keyPool 结构）、.neagent。
// 读接口永不回显明文 key，只给掩码 + api_key_set。
// ══════════════════════════════════════════════════════════
export const ModelAPI = {
  /** 读当前模型配置（key 打码）→ {providers[], model_role{}, path, ...} */
  read: () => api.get('/api/config/model'),

  /**
   * 写模型配置。
   *
   * 单 provider 形态：
   *   { provider, base_url, model?, models?, api_key? }
   *
   * ⚠ api_key 语义：不传 / 传空串 / 传含 *** 的掩码 = **保留原值**
   *   因为读接口只回掩码，前端回填的也不是真 key。这条规则保护真 key 不被覆盖。
   */
  write: (payload) => api.post('/api/config/model', payload),
};

// ══════════════════════════════════════════════════════════
// 分层条目（L4 卡片）—— 增删改查
//
// 后端签名（crates/ne-agent/src/governance/layer_store.rs，已核对）：
//   POST   /api/layers/{layer}/items        body {key, title, content}  新建
//   GET    /api/layers/{layer}/item/{key}                              读单条
//   PUT    /api/layers/{layer}/item/{key}   body {content?, title?}     改
//   DELETE /api/layers/{layer}/item/{key}                              删
//
// 重要：新建时后端会**写前快照**（snapshot_guard），路径固定在
//   02_资料库/_webui/{layer}/{key}.md
// 不覆盖既有目录结构，所以面板建的新卡与批量生成的卡在物理上分开放，
// 但都进 library_index，查询时一视同仁。
// ══════════════════════════════════════════════════════════
export const LayerAPI = {
  /** 建卡：body {key, title, content} */
  create: (layer, payload) =>
    api.post(`/api/layers/${encodeURIComponent(layer)}/items`, payload),

  /** 读单条 → {content, chars} */
  get: (layer, key) =>
    api.get(
      `/api/layers/${encodeURIComponent(layer)}/item/${encodeURIComponent(key)}`
    ),

  /** 改：body {content?, title?} */
  update: (layer, key, payload) =>
    api.put(
      `/api/layers/${encodeURIComponent(layer)}/item/${encodeURIComponent(key)}`,
      payload
    ),

  /** 删 */
  remove: (layer, key) =>
    api.del(
      `/api/layers/${encodeURIComponent(layer)}/item/${encodeURIComponent(key)}`
    ),
};

// ══════════════════════════════════════════════════════════
// 资料库统计（缺卡盘点）—— 系统视图「统计」页用
// ══════════════════════════════════════════════════════════
export const StatsAPI = {
  /** 声明 vs 实有 → {declared:{refs_total,existing_total,missing[],drift_count}, groups[]} */
  materials: () => api.get('/api/pipeline/materials'),
  /** 分层计数 → {canonical[], actual[], total, dirty_total} */
  layers: () => api.get('/api/layers/list'),
  /** 漏斗 → {progress, done, pending_count, stages[], next} */
  funnel: () => api.get('/api/pipeline/funnel'),
  /** 系统自检（POST，可能稍慢；fix=false 只查不改） */
  selfCheck: (fix = false) => api.post('/api/system/self-check', { fix }),
};