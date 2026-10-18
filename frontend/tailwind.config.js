/** TailwindCSS 配置 —— 多皮肤设计系统（UI 控件库规范 v1）
 *
 *  ══════════════════════════════════════════════════════════
 *  核心决策：色板全部桥接到 CSS 变量
 *  ══════════════════════════════════════════════════════════
 *  现有 15 个业务组件写的是 ink-900 / fg / acc / ok / warn / bad 这套旧名字。
 *  如果直接换皮肤就要改遍所有组件 —— 风险极高且容易漏。
 *
 *  这里反过来做：**保留旧 class 名字，指向规范定义的语义 Token**。
 *    ink-900 → var(--bg)        页面底
 *    ink-800 → var(--card)      卡片
 *    ink-700 → var(--card-2)    悬浮/次级
 *    ink-600 → var(--border)    描边
 *    fg      → var(--text)      主文字
 *    fg-dim  → var(--text-sec)  次文字
 *    acc     → var(--primary)   主色
 *    ok/warn/bad → success/warning/danger
 *
 *  这样切换 html[data-skin] 就瞬间穿透全部现有组件，零组件改动。
 *
 *  为支持 `bg-acc/12` 这类透明度写法，颜色走
 *  `rgb(var(--x-rgb) / <alpha-value>)`，所以 style.css 里每套皮肤
 *  同时定义 --x 与 --x-rgb 两份。
 */
export default {
  content: ['./index.html', './src/**/*.{vue,js,ts,jsx,tsx}'],
  theme: {
    extend: {
      colors: {
        // ── 语义色（规范正式名，新组件优先用这套） ──
        primary: {
          DEFAULT: 'rgb(var(--primary-rgb) / <alpha-value>)',
          hover: 'rgb(var(--primary-hover-rgb) / <alpha-value>)',
          soft: 'rgb(var(--primary-soft-rgb) / <alpha-value>)',
          'soft-hover': 'rgb(var(--primary-soft-hover-rgb) / <alpha-value>)',
        },
        'on-primary': 'rgb(var(--on-primary-rgb) / <alpha-value>)',
        surface: {
          DEFAULT: 'rgb(var(--bg-rgb) / <alpha-value>)',
          card: 'rgb(var(--card-rgb) / <alpha-value>)',
          'card-2': 'rgb(var(--card-2-rgb) / <alpha-value>)',
        },
        content: {
          DEFAULT: 'rgb(var(--text-rgb) / <alpha-value>)',
          sec: 'rgb(var(--text-sec-rgb) / <alpha-value>)',
          disabled: 'rgb(var(--text-disabled-rgb) / <alpha-value>)',
        },
        line: {
          DEFAULT: 'rgb(var(--border-rgb) / <alpha-value>)',
          divider: 'rgb(var(--divider-rgb) / <alpha-value>)',
        },
        success: {
          DEFAULT: 'rgb(var(--success-rgb) / <alpha-value>)',
          soft: 'rgb(var(--success-soft-rgb) / <alpha-value>)',
        },
        warning: {
          DEFAULT: 'rgb(var(--warning-rgb) / <alpha-value>)',
          soft: 'rgb(var(--warning-soft-rgb) / <alpha-value>)',
        },
        danger: {
          DEFAULT: 'rgb(var(--danger-rgb) / <alpha-value>)',
          soft: 'rgb(var(--danger-soft-rgb) / <alpha-value>)',
        },

        // ── 旧色板名 → 语义 Token 桥接（现有组件零改动换肤） ──
        ink: {
          900: 'rgb(var(--bg-rgb) / <alpha-value>)',
          800: 'rgb(var(--card-rgb) / <alpha-value>)',
          700: 'rgb(var(--card-2-rgb) / <alpha-value>)',
          600: 'rgb(var(--border-rgb) / <alpha-value>)',
          500: 'rgb(var(--divider-rgb) / <alpha-value>)',
        },
        fg: {
          DEFAULT: 'rgb(var(--text-rgb) / <alpha-value>)',
          dim: 'rgb(var(--text-sec-rgb) / <alpha-value>)',
        },
        acc: 'rgb(var(--primary-rgb) / <alpha-value>)',
        ok: 'rgb(var(--success-rgb) / <alpha-value>)',
        warn: 'rgb(var(--warning-rgb) / <alpha-value>)',
        bad: 'rgb(var(--danger-rgb) / <alpha-value>)',
      },
      fontFamily: {
        mono: ['SF Mono', 'JetBrains Mono', 'Menlo', 'Consolas', 'monospace'],
        sans: [
          '-apple-system',
          'BlinkMacSystemFont',
          'PingFang SC',
          'Microsoft YaHei',
          'sans-serif',
        ],
      },
      borderRadius: {
        // 圆角跟随皮肤（松绿/空紫/霞橙更饱满）
        sm: 'var(--r-sm)',
        md: 'var(--r-md)',
        lg: 'var(--r-lg)',
        xl: 'var(--r-lg)',
      },
      boxShadow: {
        // 阴影 Token 化：换肤时阴影深浅自动跟着变
        1: 'var(--shadow)',
        2: 'var(--shadow)',
        3: 'var(--shadow-hover)',
        'glow-acc': 'var(--glow)',
        'glow-ok': 'var(--glow)',
        'glow-bad': 'var(--glow)',
      },
      transitionTimingFunction: {
        ease: 'var(--ease-io)',
        'out-back': 'var(--ease-spring)',
        out: 'var(--ease-out)',
        spring: 'var(--ease-spring)',
      },
      transitionDuration: {
        1: 'var(--dur-1)',
        2: 'var(--dur-2)',
        3: 'var(--dur-3)',
        4: 'var(--dur-4)',
      },
      keyframes: {
        breathe: {
          '0%,100%': { opacity: '.45', boxShadow: '0 0 0 0 var(--ring)' },
          '50%': { opacity: '1', boxShadow: '0 0 0 6px transparent' },
        },
        flow: {
          '0%': { backgroundPosition: '200% 0' },
          '100%': { backgroundPosition: '-200% 0' },
        },
        shimmer: {
          // 规范骨架屏 shimmer
          from: { backgroundPosition: '150% 0' },
          to: { backgroundPosition: '-150% 0' },
        },
        rise: {
          from: { opacity: '0', transform: 'translateY(14px) scale(.985)' },
          to: { opacity: '1', transform: 'none' },
        },
        slideUp: {
          from: { transform: 'translateY(100%)' },
          to: { transform: 'translateY(0)' },
        },
        fadeIn: {
          from: { opacity: '0', transform: 'translateY(6px)' },
          to: { opacity: '1', transform: 'none' },
        },
        fadeUp: {
          from: { opacity: '0', transform: 'translateY(14px)' },
          to: { opacity: '1', transform: 'none' },
        },
        popIn: {
          from: { opacity: '0', transform: 'scale(.8)' },
          to: { opacity: '1', transform: 'scale(1)' },
        },
        caret: {
          '0%,49%': { opacity: '1' },
          '50%,100%': { opacity: '0' },
        },
        glowp: {
          '0%,100%': { boxShadow: 'var(--glow)' },
          '50%': { boxShadow: '0 0 24px 6px var(--ring)' },
        },
        dotb: {
          '0%,60%,100%': { transform: 'none', opacity: '.5' },
          '30%': { transform: 'translateY(-5px)', opacity: '1' },
        },
        spinb: { to: { transform: 'rotate(360deg)' } },
        shk: {
          '0%,100%': { transform: 'none' },
          '20%': { transform: 'translateX(-6px)' },
          '40%': { transform: 'translateX(5px)' },
          '60%': { transform: 'translateX(-4px)' },
          '80%': { transform: 'translateX(2px)' },
        },
        rip: { to: { transform: 'scale(1)', opacity: '0' } },
        ind: { to: { left: '110%' } },
        // 【2026-10-18 新增】呼吸圆点：用于「思考中 / 正在输出」这类持续状态。
        // 为什么不用 Tailwind 自带的 animate-pulse：
        //   1) 它作用在**文字**上是透明度 1→0.5→1，视觉上像在"闪"，很吵；
        //   2) 它没有 scale，圆点只能明灭，不像呼吸。
        // 这里同时动 opacity + scale，且首尾保持不透明，
        // 观感是"活着"而不是"故障闪烁"。
        dotbreathe: {
          '0%,100%': { opacity: '1', transform: 'scale(1)' },
          '50%': { opacity: '.3', transform: 'scale(.7)' },
        },
      },
      animation: {
        breathe: 'breathe 1.5s var(--ease-io) infinite',
        flow: 'flow 1.4s linear infinite',
        shimmer: 'shimmer 1.4s infinite',
        rise: 'rise var(--dur-3) var(--ease-out) both',
        'slide-up': 'slideUp var(--dur-3) var(--ease-out)',
        'fade-in': 'fadeIn var(--dur-2) var(--ease-out) both',
        'fade-up': 'fadeUp var(--dur-3) var(--ease-out) both',
        'pop-in': 'popIn var(--dur-3) var(--ease-spring) both',
        caret: 'caret 1s step-end infinite',
        glow: 'glowp 2.2s ease-in-out infinite',
        dotb: 'dotb 1s var(--ease-io) infinite',
        dotbreathe: 'dotbreathe 1.6s var(--ease-io) infinite',
        spinb: 'spinb 2.4s linear infinite',
        shk: 'shk .45s var(--ease-io)',
        ind: 'ind 1.3s var(--ease-io) infinite',
      },
    },
  },
  plugins: [],
}