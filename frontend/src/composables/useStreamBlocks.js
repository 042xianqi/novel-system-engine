// useStreamBlocks.js —— /api/chat 真流式读取（Phase-1 / P1 换血）
//
// 协议（后端 transport.rs :: sse_chat_stream）：
//   data: {"start":true,...}           首帧 meta
//   data: {"t":"..."}                  正文块，按序拼接
//   data: {"done":true, ...完整回复}    末帧，字段与一次性 JSON 接口 100% 一致
//
// 诚实边界：回复由后端 L1 一次性产出（LLM 层 stream:false，仓库不引入
// tokio/reqwest），这里的「真流式」= 传输层真分块（SSE），前端逐块上屏；
// 不做本地假打字机、不改生成链路。
//
// 【P1 换血 · 200ms Timer 合并节流】（抄 Operit2 StreamMarkdownRenderer
// 的 _renderTimer 思想，贴 Vue 语义重写）：
//   后端每 50ms 推一块，浏览器每块都要走一次响应式更新 → 高频小更新。
//   现在块先入 buffer，200ms 定时器统一 flush 成一次 onBlock：
//   · 每秒最多 5 次响应式写入（原来最多 20 次）
//   · 对用户不可感知（200ms 内的块肉眼就是同时到达）
//   · 流结束 / 末帧时 cancel 定时器 + 强制 flush，不丢最后一个字
//   · 兼容回退路径（非 SSE）不走节流，行为与旧版完全一致
//
// 兼容回退：响应不是 text/event-stream（旧后端 / 反代缓冲）时，
// 整体按一次性 JSON 解析，streamed=false、final 为完整对象——调用方
// 据此回落打字机，旧体验无损。

/** 渲染合并窗口（毫秒）。与 Op2 StreamMarkdownRenderer 的 200ms 对齐。 */
const FLUSH_INTERVAL_MS = 200

/**
 * 消费流式响应。
 * @param {Response} res  fetch('/api/chat?stream=1') 的原始响应
 * @param {{onBlock?: (t: string) => void}} hooks 正文块到达时回调
 *   （P1 起：200ms 内的块已合并，每次回调拿到的是合并后的文本）
 * @returns {Promise<{streamed: boolean, final: object|null}>}
 *   streamed=true 表示走了真分块；final 是末帧完整 JSON（两种模式都有）。
 */
export async function consumeChatStream(res, hooks = {}) {
  const ctype = res.headers.get('content-type') || ''

  // 非流式响应 → 一次性 JSON（旧后端 / 网关缓冲的兼容路径）
  if (!res.ok || !ctype.includes('text/event-stream')) {
    let final = null
    try { final = await res.json() } catch { /* 保持 null，调用方按失败处理 */ }
    return { streamed: false, final }
  }
  if (!res.body || typeof res.body.getReader !== 'function') {
    let final = null
    try { final = await res.json() } catch { /* ignore */ }
    return { streamed: false, final }
  }

  const reader = res.body.getReader()
  const decoder = new TextDecoder()
  let buf = ''
  let final = null

  // ── 200ms 合并节流状态 ──
  let pending = ''
  let timer = null

  const flush = () => {
    if (timer) { clearTimeout(timer); timer = null }
    if (pending) {
      const t = pending
      pending = ''
      hooks.onBlock && hooks.onBlock(t)
    }
  }

  const schedule = () => {
    if (timer) return // 已有排程，等它统一 flush（合并窗口）
    timer = setTimeout(flush, FLUSH_INTERVAL_MS)
  }

  // 处理单个 SSE 帧（帧 = 以空行分隔的一段，可能含多行，这里每帧单行 data:）
  const handleFrame = (frame) => {
    if (!frame.startsWith('data:')) return
    const payload = frame.slice(5).trim()
    if (!payload) return
    let j
    try { j = JSON.parse(payload) } catch { return }
    if (j && j.done) { final = j; return }
    if (typeof j?.t === 'string' && j.t) {
      pending += j.t
      schedule()
    }
  }

  try {
    for (;;) {
      const { value, done } = await reader.read()
      if (value) buf += decoder.decode(value, { stream: true })
      if (done) buf += decoder.decode()
      // SSE 帧以空行分隔；逐帧处理已完整到达的部分
      let sep
      while ((sep = buf.indexOf('\n\n')) !== -1) {
        const frame = buf.slice(0, sep).replace(/\r/g, '')
        buf = buf.slice(sep + 2)
        handleFrame(frame)
      }
      if (done) break
    }
    // 残余缓冲（正常不会发生：末帧后有空行；防御性处理）
    if (buf.trim()) handleFrame(buf.trim())
  } finally {
    // 无论正常结束还是中途异常，都必须收掉定时器并冲出残余文本，
    // 否则最后一个 200ms 窗口里的字会永远丢在 buffer 里。
    flush()
  }

  return { streamed: true, final }
}
