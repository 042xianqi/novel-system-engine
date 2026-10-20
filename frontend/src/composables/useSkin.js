// useSkin.js —— 主题皮肤状态
//
// 设计要点：
//   · 唯一事实源是 <html data-skin="...">（规范就是这么定的），
//     skin.css 里所有变量都挂在 html[data-skin=xxx] 上。
//   · 状态用 reactive，让「设置」页的色点能即时高亮，
//     同时把值持久化到 localStorage，下次冷启动直接套上。
//   · initSkin() 必须在 app.mount 之前调用，避免首帧闪一下默认皮肤。
import { reactive, computed } from 'vue'
import { bus, EV } from '../events.js'

export const SKINS = [
  { id: 'qinglan', name: '青蓝', desc: '冷静工具感', dot: '#4f8cff', dark: false },
  { id: 'mohei', name: '夜墨', desc: '常驻后台', dot: '#6ea0ff', dark: true },
  { id: 'songlv', name: '松绿', desc: '健康 · 效率', dot: '#10b981', dark: false },
  { id: 'kongzi', name: '空紫', desc: '创意 · AI', dot: '#8b5cf6', dark: false },
  { id: 'xiaocheng', name: '霞橙', desc: '活动 · 社交', dot: '#ef7016', dark: false },
  { id: 'xuanqing', name: '玄青', desc: 'OLED 高对比', dot: '#2dd4bf', dark: true },
]

const STORAGE_KEY = 'nov.skin'
// 2026-10-17 用户拍板：默认皮肤锁死为「夜墨」深色。
//   之前默认青蓝（浅色），用户明确说「皮肤变回浅色了」——不想要浅色默认。
const DEFAULT_SKIN = 'mohei'
// 是否允许用户手动换肤后持久化。
// 保持 true，但首次（无存储值）一律落到 mohei。
const ALLOW_USER_OVERRIDE = true

/** 当前皮肤（响应式，供 UI 高亮） */
export const skinState = reactive({
  current: DEFAULT_SKIN,
})

/** 当前皮肤是否深色（决定个别组件的明暗取舍） */
export const isDarkSkin = computed(() => {
  const s = SKINS.find((x) => x.id === skinState.current)
  return !!s?.dark
})

/** 把皮肤真正写进 DOM */
function apply(skinId) {
  if (typeof document === 'undefined') return
  document.documentElement.setAttribute('data-skin', skinId)
  // 顺手同步 color-scheme，让浏览器原生控件（滚动条/输入光标）
  // 也跟着变成深色，否则玄青皮肤下输入框光标是黑的，看不见
  const s = SKINS.find((x) => x.id === skinId)
  document.documentElement.style.colorScheme = s?.dark ? 'dark' : 'light'
}

/** 冷启动时调用：读存储 → 套皮肤。必须在 mount 之前。 */
export function initSkin() {
  let saved = null
  try {
    saved = localStorage.getItem(STORAGE_KEY)
  } catch (e) {
    // 隐私模式 / 沙箱禁 localStorage 时静默降级
  }
  // 只有存过且合法、且用户确实手动选过，才用它。
  // 否则一律落到 DEFAULT_SKIN（夜墨）——
  // 这样「之前存过浅色」的老状态不会把新默认顶掉。
  const valid =
    ALLOW_USER_OVERRIDE && SKINS.some((s) => s.id === saved) ? saved : DEFAULT_SKIN
  skinState.current = valid
  apply(valid)
  // 把最终值写回存储，保证下次启动读到的就是它
  try {
    localStorage.setItem(STORAGE_KEY, valid)
  } catch (e) {
    /* 忽略 */
  }
  return valid
}


/** 切换皮肤（幂等：同值不重复写 DOM） */
export function setSkin(skinId) {
  if (!SKINS.some((s) => s.id === skinId)) return false
  if (skinState.current === skinId) return true
  skinState.current = skinId
  apply(skinId)
  try {
    localStorage.setItem(STORAGE_KEY, skinId)
  } catch (e) {
    // 存不了就算了，本次会话内仍然生效
  }
  // 广播出去，方便别处（如强制重绘 canvas）监听
  if (typeof window !== 'undefined') {
    bus.emit(EV.SKIN_CHANGED, skinId)
  }
  return true
}