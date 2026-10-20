<script setup>
// MarkdownBlock.vue —— AI 消息的 Markdown 渲染块
// ══════════════════════════════════════════════════════════════
// 职责：把一段 Markdown 文本渲染成安全 HTML。
//
// 安全策略（硬要求，禁止直接 v-html 未净化内容）：
//   1. marked 的 renderer.html 被覆写为「转义原始 HTML」——
//      用户/AI 写的 <script> 只会作为字面文本出现，不会变成节点。
//   2. 即便这样，输出仍过一遍白名单净化器 sanitizeHtml()：
//      只保留允许的标签，只复制白名单属性，href/src 校验协议。
//   3. 双保险之后才交给 v-html。
//
// 代码块：不走 v-html，而是用 marked.lexer 识别 type==='code' 的块，
//   交给本组件的模板单独渲染（等宽 + 深色底 + 语言角标 + 复制按钮）。
//   这样才能挂真正的点击事件（v-html 里的事件处理器不会生效）。
//
// 性能：lexer/parse 结果做一层按字符串的缓存；历史（静态）消息会命中。
// ══════════════════════════════════════════════════════════════

import { computed, ref } from 'vue'
import { marked } from 'marked'
import hljs from 'highlight.js/lib/core'
import javascript from 'highlight.js/lib/languages/javascript'
import typescript from 'highlight.js/lib/languages/typescript'
import json from 'highlight.js/lib/languages/json'
import bash from 'highlight.js/lib/languages/bash'
import shell from 'highlight.js/lib/languages/shell'
import python from 'highlight.js/lib/languages/python'
import rust from 'highlight.js/lib/languages/rust'
import xml from 'highlight.js/lib/languages/xml'
import css from 'highlight.js/lib/languages/css'
import markdown from 'highlight.js/lib/languages/markdown'
import sql from 'highlight.js/lib/languages/sql'
import go from 'highlight.js/lib/languages/go'
import yaml from 'highlight.js/lib/languages/yaml'

// 按需注册语言（只打包用得到的高亮语法，别把整包 highlight.js 拉进来）
const LANGS = {
  javascript, typescript, json, bash, shell, python, rust,
  xml, css, markdown, sql, go, yaml,
}
for (const [name, def] of Object.entries(LANGS)) hljs.registerLanguage(name, def)
hljs.registerAliases(['js', 'jsx', 'mjs', 'cjs'], { languageName: 'javascript' })
hljs.registerAliases(['ts', 'tsx'], { languageName: 'typescript' })
hljs.registerAliases(['sh', 'zsh', 'console'], { languageName: 'bash' })
hljs.registerAliases(['html', 'vue', 'svg'], { languageName: 'xml' })
hljs.registerAliases(['yml'], { languageName: 'yaml' })
hljs.registerAliases(['md'], { languageName: 'markdown' })
hljs.registerAliases(['py'], { languageName: 'python' })
hljs.registerAliases(['rs'], { languageName: 'rust' })

const props = defineProps({
  source: { type: String, default: '' },
})

// ── HTML 实体转义 ──
function escapeHtml(s) {
  return String(s)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;')
}

// ── marked：禁用原始 HTML（转义成字面文本） ──
marked.use({
  gfm: true,
  breaks: true,
  renderer: {
    // marked v18 的回调参数是 token 对象；老版本可能是字符串。两种都兜住。
    html(token) {
      const raw =
        typeof token === 'string' ? token : token?.text ?? token?.raw ?? ''
      return escapeHtml(raw)
    },
  },
})

// ══════════════════════════════════════════════ 白名单净化器
const ALLOWED = {
  h1: [], h2: [], h3: [], h4: [], h5: [], h6: [],
  p: [], br: [], hr: [],
  strong: [], b: [], em: [], i: [], del: [], s: [], mark: [],
  code: ['class'], pre: ['class'], kbd: [],
  ul: [], ol: ['start'], li: [],
  blockquote: [],
  a: ['href', 'title'],
  table: [], thead: [], tbody: [], tr: [], th: ['align'], td: ['align'],
  span: ['class'],
  img: ['src', 'alt', 'title'],
}

// 这些标签连同后代一起删掉（脱壳会泄露脚本正文）
const DROP_ENTIRELY = new Set([
  'script', 'style', 'iframe', 'object', 'embed', 'link', 'meta',
  'base', 'form', 'input', 'button', 'select', 'textarea',
  'svg', 'math', 'template', 'noscript', 'frame', 'frameset',
])

// 合法 URL：只允许这些协议 / 相对路径；javascript:、vbscript:、data: 一律拒绝
function safeUrl(raw, allowDataImage = false) {
  const u = String(raw ?? '').trim()
  if (!u) return null
  const flat = u.replace(/[\u0000-\u001f\u007f]/g, '') // 防御 `java\nscript:`
  if (/^(https?:|mailto:|tel:)/i.test(flat)) return flat
  if (flat.startsWith('#') || flat.startsWith('/') || flat.startsWith('./') || flat.startsWith('../')) return flat
  if (allowDataImage && /^data:image\/(png|jpeg|jpg|gif|webp);base64,/i.test(flat)) return flat
  return null
}

function cleanAttrs(el, tag) {
  const allowed = ALLOWED[tag] || []
  for (const attr of [...el.attributes]) {
    const name = attr.name.toLowerCase()
    if (!allowed.includes(name)) {
      el.removeAttribute(attr.name)
      continue
    }
    if (name === 'href' || name === 'src') {
      const ok = safeUrl(attr.value, tag === 'img')
      if (!ok) el.removeAttribute(attr.name)
      else el.setAttribute(name, ok)
    }
    // class 只放行高亮 / 语言类，防止样式注入
    if (name === 'class' && !/^(hljs[\w-]*|language-[\w#+-]+)$/.test(attr.value.trim())) {
      el.removeAttribute(name)
    }
    if (name === 'align' && !/^(left|center|right|justify)$/i.test(attr.value)) {
      el.removeAttribute(name)
    }
  }
}

function walk(node) {
  for (const child of [...node.childNodes]) {
    if (child.nodeType === 8) { // 注释
      child.remove()
      continue
    }
    if (child.nodeType !== 1) continue // 文本节点放行

    const tag = child.tagName.toLowerCase()

    if (DROP_ENTIRELY.has(tag)) {
      child.remove()
      continue
    }
    if (!Object.prototype.hasOwnProperty.call(ALLOWED, tag)) {
      child.replaceWith(...child.childNodes) // 未知无害标签：脱壳保留文字
      continue
    }

    cleanAttrs(child, tag)
    walk(child)

    if (tag === 'a') {
      const href = child.getAttribute('href')
      if (href && /^https?:/i.test(href)) {
        child.setAttribute('target', '_blank')
        child.setAttribute('rel', 'noopener noreferrer nofollow')
      }
    }
  }
}

/** 白名单净化：输入 HTML → 安全 HTML。无 DOM 环境退化为纯文本转义。 */
function sanitizeHtml(html) {
  if (typeof document === 'undefined' || !document.createElement) {
    return escapeHtml(html)
  }
  const tpl = document.createElement('template')
  tpl.innerHTML = String(html)
  walk(tpl.content)
  return tpl.innerHTML
}

// ══════════════════════════════════════════════ 解析
let cacheKey = null
let cacheVal = []

function buildBlocks(src) {
  if (src === cacheKey) return cacheVal
  const tokens = marked.lexer(String(src || ''))
  const out = []
  for (let i = 0; i < tokens.length; i++) {
    const t = tokens[i]
    if (t.type === 'space') continue
    if (t.type === 'code') {
      out.push({ kind: 'code', raw: t.text ?? '', lang: (t.lang || '').trim().split(/\s+/)[0], key: i })
    } else {
      let html = ''
      try {
        html = marked.parser([t])
      } catch (e) {
        html = escapeHtml(t.raw || '')
      }
      out.push({ kind: 'html', html: sanitizeHtml(html), key: i })
    }
  }
  cacheKey = src
  cacheVal = out
  return out
}

const blocks = computed(() => buildBlocks(props.source))

// ══════════════════════════════════════════════ 代码块高亮 + 复制
function highlight(code, lang) {
  const l = (lang || '').toLowerCase()
  try {
    if (l && hljs.getLanguage(l)) {
      return sanitizeHtml(hljs.highlight(code, { language: l, ignoreIllegals: true }).value)
    }
    if (!l) {
      return sanitizeHtml(hljs.highlightAuto(code).value) // 未标语言：自动检测
    }
  } catch (e) {
    /* 落到下面的转义分支 */
  }
  return escapeHtml(code)
}

function highlighted(b) {
  return highlight(b.raw, b.lang)
}

function displayLang(b) {
  const l = (b.lang || '').toLowerCase()
  if (!l) return 'text'
  const alias = { js: 'javascript', ts: 'typescript', sh: 'bash', html: 'xml', vue: 'xml', py: 'python', rs: 'rust', yml: 'yaml', md: 'markdown' }
  return alias[l] || l
}

// 每块独立的「已复制」状态：{ [blockKey]: true }
const copied = ref({})
const copyTimers = {}

async function doCopy(b) {
  const text = b.raw
  let ok = false
  try {
    if (typeof navigator !== 'undefined' && navigator.clipboard && navigator.clipboard.writeText) {
      await navigator.clipboard.writeText(text)
      ok = true
    }
  } catch (e) {
    ok = false
  }
  if (!ok) ok = legacyCopy(text) // 降级：非安全上下文 / 权限被拒
  if (!ok) return

  copied.value = { ...copied.value, [b.key]: true }
  clearTimeout(copyTimers[b.key])
  copyTimers[b.key] = setTimeout(() => {
    const next = { ...copied.value }
    delete next[b.key]
    copied.value = next
  }, 1500)
}

/** execCommand 降级方案（http:// 非安全上下文里 navigator.clipboard 不可用） */
function legacyCopy(text) {
  try {
    if (typeof document === 'undefined') return false
    const ta = document.createElement('textarea')
    ta.value = text
    ta.setAttribute('readonly', '')
    ta.style.position = 'fixed'
    ta.style.top = '-9999px'
    ta.style.opacity = '0'
    document.body.appendChild(ta)
    ta.select()
    ta.setSelectionRange(0, ta.value.length)
    const ok = document.execCommand('copy')
    document.body.removeChild(ta)
    return ok
  } catch (e) {
    return false
  }
}
</script>

<template>
  <div class="md-body">
    <template v-for="b in blocks" :key="b.key">
      <!-- 代码块：等宽 + 深色底 + 语言角标 + 复制 -->
      <div v-if="b.kind === 'code'" class="md-code">
        <div class="md-code-bar">
          <span class="md-code-lang">{{ displayLang(b) }}</span>
          <button
            type="button"
            class="md-code-copy"
            :class="{ 'is-copied': copied[b.key] }"
            :aria-label="copied[b.key] ? '已复制' : '复制代码'"
            @click="doCopy(b)"
          >
            {{ copied[b.key] ? '已复制' : '复制' }}
          </button>
        </div>
        <pre class="md-code-pre"><code class="hljs" v-html="highlighted(b)"></code></pre>
      </div>

      <!-- 其余块：已净化 HTML -->
      <div v-else class="md-block" v-html="b.html"></div>
    </template>
  </div>
</template>

<style scoped>
/* 代码块强制深色底 + 等宽字体（任务硬要求，不受皮肤影响） */
.md-code {
  position: relative;
  margin: 0.55rem 0;
  border-radius: 10px;
  overflow: hidden;
  background: #0d1117; /* 刻意固定深色：代码块在所有皮肤下都应可读 */
  border: 1px solid rgba(255, 255, 255, 0.09);
}
.md-code-bar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 0.5rem;
  padding: 0.3rem 0.5rem 0.3rem 0.65rem;
  background: rgba(255, 255, 255, 0.05);
  border-bottom: 1px solid rgba(255, 255, 255, 0.08);
}
.md-code-lang {
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  font-size: 11px;
  letter-spacing: 0.02em;
  color: rgba(230, 237, 243, 0.72);
}
.md-code-copy {
  font-size: 11px;
  line-height: 1;
  padding: 4px 8px;
  border-radius: 6px;
  cursor: pointer;
  color: rgba(230, 237, 243, 0.82);
  background: rgba(255, 255, 255, 0.07);
  border: 1px solid rgba(255, 255, 255, 0.1);
  transition: background 160ms ease, color 160ms ease, border-color 160ms ease;
}
.md-code-copy:hover {
  background: rgba(255, 255, 255, 0.14);
}
.md-code-copy.is-copied {
  color: #7ee2b8;
  border-color: rgba(126, 226, 184, 0.4);
  background: rgba(126, 226, 184, 0.12);
}
.md-code-pre {
  margin: 0;
  padding: 0.7rem 0.85rem;
  overflow-x: auto;
}
.md-code-pre code {
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, "Courier New", monospace;
  font-size: 12.5px;
  line-height: 1.6;
  color: #e6edf3;
  background: transparent;
}

/* ── 正文块 ── */
.md-block :deep(*) { max-width: 100%; }
.md-block :deep(p) { margin: 0.28rem 0; }
.md-block :deep(p:first-child) { margin-top: 0; }
.md-block :deep(p:last-child) { margin-bottom: 0; }
.md-block :deep(h1),
.md-block :deep(h2),
.md-block :deep(h3),
.md-block :deep(h4),
.md-block :deep(h5),
.md-block :deep(h6) {
  margin: 0.7rem 0 0.35rem;
  font-weight: 600;
  line-height: 1.35;
}
.md-block :deep(h1) { font-size: 1.35em; }
.md-block :deep(h2) { font-size: 1.2em; }
.md-block :deep(h3) { font-size: 1.08em; }
.md-block :deep(h4),
.md-block :deep(h5),
.md-block :deep(h6) { font-size: 1em; }
.md-block :deep(h1:first-child),
.md-block :deep(h2:first-child),
.md-block :deep(h3:first-child) { margin-top: 0; }

.md-block :deep(ul),
.md-block :deep(ol) {
  margin: 0.35rem 0;
  padding-left: 1.35rem;
}
.md-block :deep(ul) { list-style: disc; }
.md-block :deep(ol) { list-style: decimal; }
.md-block :deep(li) { margin: 0.16rem 0; }
.md-block :deep(li > ul),
.md-block :deep(li > ol) { margin: 0.16rem 0; }

.md-block :deep(blockquote) {
  margin: 0.45rem 0;
  padding: 0.2rem 0.7rem;
  border-left: 3px solid rgb(var(--primary-rgb) / 0.55);
  opacity: 0.88;
}

/* 行内代码（块级代码在 .md-code 里单独处理） */
.md-block :deep(code) {
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  font-size: 0.9em;
  padding: 0.1em 0.38em;
  border-radius: 5px;
  background: rgba(127, 127, 127, 0.18);
}

/* 兜底：列表/引用里嵌套的 pre 也走深色等宽（无复制按钮，但样式一致） */
.md-block :deep(pre) {
  margin: 0.5rem 0;
  padding: 0.7rem 0.85rem;
  border-radius: 10px;
  overflow-x: auto;
  background: #0d1117;
  border: 1px solid rgba(255, 255, 255, 0.09);
}
.md-block :deep(pre code) {
  padding: 0;
  background: transparent;
  color: #e6edf3;
  font-size: 12.5px;
  line-height: 1.6;
}

.md-block :deep(a) {
  color: rgb(var(--primary-rgb));
  text-decoration: underline;
  text-underline-offset: 2px;
  word-break: break-all;
}
.md-block :deep(a:hover) { opacity: 0.82; }

.md-block :deep(hr) {
  margin: 0.8rem 0;
  border: 0;
  border-top: 1px solid rgba(255, 255, 255, 0.14);
}

.md-block :deep(table) {
  width: 100%;
  margin: 0.5rem 0;
  border-collapse: collapse;
  font-size: 0.94em;
}
.md-block :deep(th),
.md-block :deep(td) {
  padding: 0.3rem 0.5rem;
  border: 1px solid rgba(255, 255, 255, 0.16);
  text-align: left;
}
.md-block :deep(th) { font-weight: 600; }

.md-block :deep(strong) { font-weight: 600; }
.md-block :deep(img) { max-width: 100%; border-radius: 8px; }

/* ── highlight.js 主题（内联，避免引入整包 css 污染全局）── */
.md-code :deep(.hljs-comment),
.md-code :deep(.hljs-quote) { color: #8b949e; font-style: italic; }
.md-code :deep(.hljs-keyword),
.md-code :deep(.hljs-selector-tag),
.md-code :deep(.hljs-meta),
.md-code :deep(.hljs-doctag) { color: #ff7b72; }
.md-code :deep(.hljs-string),
.md-code :deep(.hljs-regexp),
.md-code :deep(.hljs-addition) { color: #a5d6ff; }
.md-code :deep(.hljs-number),
.md-code :deep(.hljs-literal),
.md-code :deep(.hljs-symbol),
.md-code :deep(.hljs-bullet) { color: #79c0ff; }
.md-code :deep(.hljs-title),
.md-code :deep(.hljs-section),
.md-code :deep(.hljs-name),
.md-code :deep(.hljs-selector-id),
.md-code :deep(.hljs-selector-class) { color: #d2a8ff; }
.md-code :deep(.hljs-attr),
.md-code :deep(.hljs-attribute),
.md-code :deep(.hljs-variable),
.md-code :deep(.hljs-template-variable) { color: #ffa657; }
.md-code :deep(.hljs-built_in),
.md-code :deep(.hljs-type),
.md-code :deep(.hljs-class .hljs-title) { color: #ffa657; }
.md-code :deep(.hljs-tag) { color: #7ee787; }
.md-code :deep(.hljs-deletion) { color: #ffa198; }
.md-code :deep(.hljs-emphasis) { font-style: italic; }
.md-code :deep(.hljs-strong) { font-weight: 600; }
.md-code :deep(.hljs-link) { color: #a5d6ff; text-decoration: underline; }
</style>
