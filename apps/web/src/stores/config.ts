import { defineStore } from 'pinia'
import { ref, computed, onScopeDispose } from 'vue'
import type { PermissionMode, ReasoningEffort } from '@/protocol/commands'
import { apiGet, apiSend } from '@/bridge/http'

export interface ProviderConfig {
  id: string; name: string; apiKeyMasked: string; hasKey?: boolean
  models: string[]; baseUrl?: string
  type?: string; model?: string; fastModel?: string | null; toolProtocol?: string
  contextWindow?: number
  modelContextWindows?: Record<string, number>
  supportsWebSearch?: boolean
  supportsVision?: boolean
  active?: boolean
  builtin?: boolean
  status?: ProviderStatus
  modelDescriptions?: Record<string, string>
  modelParameters?: Record<string, ModelParameters>
  capabilityOverrides?: Record<string, CapabilityOverride>
}

export interface ModelParameters {
  temperature?: number
  topK?: number
  topP?: number
  maxOutputTokens?: number
  reasoningEffort?: string
  reasoningField?: string
  reasoningMapping?: Partial<Record<'low' | 'medium' | 'high' | 'xhigh', string>>
}
export interface CapabilityOverride { text?: boolean; vision?: boolean; image_generation?: boolean; reasoning?: boolean }
export interface SubAgentConfig { id: string; providerId: string; model: string; description?: string }
export interface SubAgentSettings { agents: SubAgentConfig[]; fallbackId?: string; maxAgents: number }

export type ProviderProtocol = 'openai_compatible' | 'openai_responses' | 'anthropic_messages' | 'gemini_native'
export type ProviderStatus = 'unconfigured' | 'configured' | 'current'

export interface ConnectionSettings {
  providerRetryCount: number
  wsRetryCount: number
  reconnectInitialDelayMs: number
  reconnectMaxDelayMs: number
  maxConcurrentTasks: number
  autoCompactPercent: number
}

export const DEFAULT_CONNECTION_SETTINGS: ConnectionSettings = {
  providerRetryCount: 2,
  wsRetryCount: 10,
  reconnectInitialDelayMs: 500,
  reconnectMaxDelayMs: 10_000,
  maxConcurrentTasks: 5,
  autoCompactPercent: 80,
}

export interface ProviderPreset {
  id: string
  name: string
  baseUrl: string
  protocol: ProviderProtocol
}

export const BUILTIN_PROVIDER_PRESETS: ProviderPreset[] = [
  { id: 'deepseek', name: 'DeepSeek', baseUrl: 'https://api.deepseek.com/v1', protocol: 'openai_compatible' },
  { id: 'zhipu', name: '智谱', baseUrl: 'https://open.bigmodel.cn/api/paas/v4', protocol: 'openai_compatible' },
  { id: 'minimax', name: 'MiniMax', baseUrl: 'https://api.minimaxi.com/v1', protocol: 'openai_compatible' },
  { id: 'openai', name: 'OpenAI', baseUrl: 'https://api.openai.com/v1', protocol: 'openai_responses' },
  { id: 'anthropic', name: 'Anthropic', baseUrl: 'https://api.anthropic.com/v1', protocol: 'anthropic_messages' },
  { id: 'google', name: 'Gemini', baseUrl: 'https://generativelanguage.googleapis.com/v1beta', protocol: 'gemini_native' },
  { id: 'opencode', name: 'OpenCode', baseUrl: 'https://opencode.ai/zen/go/v1', protocol: 'openai_compatible' },
]

export interface ProviderInput {
  id: string; name: string; apiKey: string; apiKeys?: string[]; models: string[]
  baseUrl?: string; type?: string; toolProtocol?: string; contextWindow?: number
  modelContextWindows?: Record<string, number>
  fastModel?: string | null; activate?: boolean; supportsWebSearch?: boolean; supportsVision?: boolean
  modelDescriptions?: Record<string, string>; modelParameters?: Record<string, ModelParameters>
  capabilityOverrides?: Record<string, CapabilityOverride>
  headers?: Record<string, string>
}

export function providerStatus(provider: ProviderConfig, activeId: string): ProviderStatus {
  const configured = Boolean(provider.hasKey && provider.models.length > 0)
  if (configured && provider.id === activeId) return 'current'
  return configured ? 'configured' : 'unconfigured'
}

export function mergeProviderList(configured: ProviderConfig[], activeId: string): ProviderConfig[] {
  const configuredById = new Map(configured.map(provider => [provider.id, provider]))
  const builtInIds = new Set(BUILTIN_PROVIDER_PRESETS.map(preset => preset.id))
  const builtIns = BUILTIN_PROVIDER_PRESETS.map(preset => {
    const saved = configuredById.get(preset.id)
    const provider: ProviderConfig = {
      id: preset.id,
      name: saved?.name || preset.name,
      apiKeyMasked: saved?.apiKeyMasked || '',
      hasKey: Boolean(saved?.hasKey),
      models: saved?.models ?? [],
      baseUrl: saved?.baseUrl || preset.baseUrl,
      type: saved?.type || preset.protocol,
      model: saved?.model,
      fastModel: saved?.fastModel,
      toolProtocol: saved?.toolProtocol || preset.protocol,
      contextWindow: saved?.contextWindow ?? 256000,
      modelContextWindows: { ...(saved?.modelContextWindows ?? {}) },
      supportsWebSearch: saved?.supportsWebSearch ?? false,
      supportsVision: saved?.supportsVision ?? false,
      modelDescriptions: { ...(saved?.modelDescriptions ?? {}) },
      modelParameters: { ...(saved?.modelParameters ?? {}) },
      capabilityOverrides: { ...(saved?.capabilityOverrides ?? {}) },
      active: activeId === preset.id,
      builtin: true,
    }
    provider.status = providerStatus(provider, activeId)
    return provider
  })
  const custom = configured
    .filter(provider => !builtInIds.has(provider.id))
    .map(provider => ({ ...provider, builtin: false, status: providerStatus(provider, activeId) }))
  return [...builtIns, ...custom]
}

export const PERMISSION_MODES: { mode: PermissionMode; label: string; desc: string }[] = [
  { mode: 'ask', label: '询问', desc: '工作区内编辑自动执行，命令及删除、移动操作需确认' },
  { mode: 'auto', label: '自动', desc: '读写自动放行，仅破坏性需确认' },
  { mode: 'full', label: '放行', desc: '全部自动执行（仅信任场景）' },
  { mode: 'minimal', label: '极简', desc: '只提供 shell 工具，适合纯命令行任务' },
]

export type ThemeMode = 'system' | 'light' | 'dark' | 'book' | 'orange' | 'ink' | 'abyss' | 'ember' | 'celadon' | 'linen' | 'terminal-dark' | 'terminal-light'
const THEME_VALUES: ThemeMode[] = ['system', 'light', 'dark', 'book', 'orange', 'ink', 'abyss', 'ember', 'celadon', 'linen', 'terminal-dark', 'terminal-light']
export const THEME_MODES: { mode: ThemeMode; label: string; desc: string }[] = [
  { mode: 'system', label: '跟随系统', desc: '与手机系统深浅色保持一致' },
  { mode: 'light', label: '明亮模式', desc: '始终使用浅色界面' },
  { mode: 'dark', label: '夜间模式', desc: '始终使用深色界面' },
  { mode: 'book', label: '书卷纸', desc: '柔和纸张底色与墨绿色点缀' },
  { mode: 'orange', label: '橙白', desc: '明快白色底面与暖橙色点缀' },
  { mode: 'ink', label: '墨玉', desc: '墨黑底面与温润玉绿色点缀' },
  { mode: 'abyss', label: '深海', desc: '深海蓝黑底面与清冷青蓝点缀' },
  { mode: 'ember', label: '炭褐', desc: '炭黑褐底面与余烬铜色点缀' },
  { mode: 'celadon', label: '青瓷', desc: '青瓷浅灰底面与釉绿色点缀' },
  { mode: 'terminal-dark', label: '终端 · 黑', desc: '全等宽字体；思考过程白色，输出与工具调用橙色' },
  { mode: 'terminal-light', label: '终端 · 白', desc: '全等宽字体；一律黑字，无彩色点缀' },
  { mode: 'linen', label: '亚麻', desc: '自然亚麻白底面与沉静靛色点缀' },
]

export const REASONING_EFFORTS: { value: ReasoningEffort; label: string }[] = [
  { value: 'off', label: '关闭' },
  { value: 'auto', label: '自动' },
  { value: 'low', label: '低' },
  { value: 'medium', label: '中' },
  { value: 'high', label: '高' },
  { value: 'xhigh', label: '超高' },
]

/** 取当前主题档位：优先 Android 原生偏好（JS 桥），其次 localStorage，默认跟随系统。 */
export function readThemeMode(): ThemeMode {
  const bridge = (window as any).CoomiAndroid
  if (bridge && typeof bridge.getThemeMode === 'function') {
    try {
      const v = String(bridge.getThemeMode() ?? '')
      // 旧版单一 terminal 档位迁移到新的黑版终端。
      if (v === 'terminal') return 'terminal-dark'
      if (THEME_VALUES.includes(v as ThemeMode)) return v as ThemeMode
    } catch { /* 桥未就绪时走 localStorage */ }
  }
  const saved = localStorage.getItem('coomi.themeMode')
  if (saved === 'terminal') return 'terminal-dark'
  return THEME_VALUES.includes(saved as ThemeMode) ? saved as ThemeMode : 'system'
}

export type MottoFont = 'default' | 'guofeng'

function readMottoFont(): MottoFont {
  try {
    const bridge = window.CoomiAndroid
    if (bridge?.getAppearanceConfig) {
      const appearance = JSON.parse(bridge.getAppearanceConfig())
      return appearance.mottoFont === 'guofeng' ? 'guofeng' : 'default'
    }
  } catch { /* 原生桥未就绪时使用本地缓存 */ }
  return localStorage.getItem('coomi.mottoFont') === 'guofeng' ? 'guofeng' : 'default'
}

/** 写入 <html data-theme>，前端 global.css 据此切换暗色主题。 */
export function applyTheme(mode: ThemeMode) {
  // 终端两版自带明确深浅色，不参与 system 的深浅色推断。
  if (mode === 'terminal-dark' || mode === 'terminal-light') {
    document.documentElement.setAttribute('data-theme', mode)
    return
  }
  const dark = mode === 'dark'
    || (mode === 'system' && window.matchMedia?.('(prefers-color-scheme: dark)').matches)
  document.documentElement.setAttribute('data-theme', dark ? 'dark' : mode === 'system' ? 'light' : mode)
}

// 浏览器独立开发时的兜底数据（后端不可达时使用）
const MOCK_PROVIDERS: ProviderConfig[] = [
  { id: 'openai', name: 'OpenAI', apiKeyMasked: '****a1b2', hasKey: true, models: ['gpt-4o', 'gpt-4o-mini'], baseUrl: 'https://api.openai.com/v1' },
  { id: 'anthropic', name: 'Anthropic', apiKeyMasked: '****9f3c', hasKey: true, models: ['claude-sonnet-4', 'claude-opus-4'] },
]

export function displayModelName(name: string): string {
  if (!name) return name
  const cleaned = name.replace(/[\0--​-‏‪-‮⁦-⁩︀-️ﹰ-﻿￰-￿]/g, '')
  return cleaned || name
}

export const useConfigStore = defineStore('config', () => {
  const savedPermission = localStorage.getItem('coomi.permissionMode') as PermissionMode | null
  const ttsAutoRead = ref(localStorage.getItem('coomi.ttsAutoRead') === '1')
  const ttsRate = ref(readStoredRate())
  function setTtsAutoRead(enabled: boolean) {
    ttsAutoRead.value = enabled
    localStorage.setItem('coomi.ttsAutoRead', enabled ? '1' : '0')
  }

  function readStoredRate(): number {
    const value = Number(localStorage.getItem('coomi.ttsRate'))
    return Number.isFinite(value) && value >= 0.5 && value <= 2 ? value : 1
  }
  function setTtsRate(rate: number) {
    const clamped = Math.min(2, Math.max(0.5, rate))
    ttsRate.value = clamped
    localStorage.setItem('coomi.ttsRate', String(clamped))
  }

  const savedStartupPermission = localStorage.getItem('coomi.defaultPermissionMode') as PermissionMode | null
  const permissionMode = ref<PermissionMode>(['ask', 'auto', 'full', 'minimal'].includes(savedStartupPermission ?? '') ? savedStartupPermission! : ['ask', 'auto', 'full', 'minimal'].includes(savedPermission ?? '') ? savedPermission! : 'ask')
  const planMode = ref(false)
  const savedProduction = (localStorage.getItem('coomi.productionMode') || 'normal') as 'normal' | 'overload' | 'berserk'
  const productionMode = ref<'normal' | 'overload' | 'berserk'>(['normal','overload','berserk'].includes(savedProduction) ? savedProduction : 'normal')
  const savedDefaultMode = localStorage.getItem('coomi.defaultPermissionMode') as PermissionMode | null
  const defaultPermissionMode = ref<PermissionMode>(['ask', 'auto', 'full', 'minimal'].includes(savedDefaultMode ?? '') ? savedDefaultMode! : permissionMode.value)
  const sendMorphAnimation = ref(localStorage.getItem('coomi.sendMorphAnimation') !== '0')
  /** 关闭所有动画：总开关。关闭后禁用全部 CSS/GSAP/Morphicons/Web Animations，并强制关闭液滴动画。 */
  const allAnimationsOff = ref(localStorage.getItem('coomi.allAnimationsOff') === '1')
  if (allAnimationsOff.value) {
    sendMorphAnimation.value = false
    localStorage.setItem('coomi.sendMorphAnimation', '0')
  }
  /** 极简界面模式：工具调用折叠成一小块方框（点开才看详情），文字缩小。 */
  const minimalUi = ref(localStorage.getItem('coomi.minimalUi') !== '0')
  const themeMode = ref<ThemeMode>(readThemeMode())
  const mottoFont = ref<MottoFont>(readMottoFont())
  function syncMottoFont(event: Event) {
    const value = (event as CustomEvent<AppearanceConfig>).detail?.mottoFont
    mottoFont.value = value === undefined ? readMottoFont() : value === 'guofeng' ? 'guofeng' : 'default'
    localStorage.setItem('coomi.mottoFont', mottoFont.value)
  }
  window.addEventListener('coomi:appearance-changed', syncMottoFont)
  onScopeDispose(() => window.removeEventListener('coomi:appearance-changed', syncMottoFont))
  function setMottoFont(value: MottoFont) {
    mottoFont.value = value === 'guofeng' ? 'guofeng' : 'default'
    localStorage.setItem('coomi.mottoFont', mottoFont.value)
    try { window.CoomiAndroid?.setMottoFont?.(mottoFont.value) } catch { /* 本地选择仍可用 */ }
  }
  const savedEffort = localStorage.getItem('coomi.reasoningEffort') as ReasoningEffort | null
  const reasoningEffort = ref<ReasoningEffort>(REASONING_EFFORTS.some(item => item.value === savedEffort) ? savedEffort! : 'auto')
  const savedRounds = Number(localStorage.getItem('coomi.maxToolRounds'))
  const maxToolRounds = ref([192, 256, 512].includes(savedRounds) ? savedRounds : 192)
  const subAgentSettings = ref<SubAgentSettings>({ agents: [], maxAgents: 20 })
  const connectionSettings = ref<ConnectionSettings>({
    providerRetryCount: readStoredInt('coomi.providerRetryCount', 0, 10, DEFAULT_CONNECTION_SETTINGS.providerRetryCount),
    wsRetryCount: readStoredInt('coomi.wsRetryCount', 0, 100, DEFAULT_CONNECTION_SETTINGS.wsRetryCount),
    reconnectInitialDelayMs: readStoredInt('coomi.reconnectInitialDelayMs', 500, 60_000, DEFAULT_CONNECTION_SETTINGS.reconnectInitialDelayMs),
    reconnectMaxDelayMs: readStoredInt('coomi.reconnectMaxDelayMs', 1_000, 120_000, DEFAULT_CONNECTION_SETTINGS.reconnectMaxDelayMs),
    maxConcurrentTasks: readStoredInt('coomi.maxConcurrentTasks', 1, 20, DEFAULT_CONNECTION_SETTINGS.maxConcurrentTasks),
    autoCompactPercent: readStoredInt('coomi.autoCompactPercent', 10, 95, DEFAULT_CONNECTION_SETTINGS.autoCompactPercent),
  })

  const providers = ref<ProviderConfig[]>([])
  const activeId = ref('')
  const loading = ref(false)
  const usingMock = ref(false)
  const lastError = ref<string | null>(null)

  const currentProviderId = ref('')
  const currentModel = ref('')
  const currentProvider = computed(() => providers.value.find(p => p.id === currentProviderId.value) ?? null)
  const mergedProviders = computed(() => mergeProviderList(providers.value, activeId.value))

  function applyList(list: ProviderConfig[], active: string) {
    providers.value = list
    activeId.value = active
    // 同步当前选择：优先 active，其次第一个
    const sel = list.find(p => p.id === active) ?? list[0]
    if (sel) {
      const savedProvider = localStorage.getItem('coomi.providerId')
      const savedModel = localStorage.getItem('coomi.model')
      const saved = list.find(p => p.id === savedProvider && p.models.includes(savedModel ?? ''))
      currentProviderId.value = saved?.id ?? sel.id
      currentModel.value = savedModel && saved ? savedModel : (sel.model || sel.models[0] || '')
    } else {
      currentProviderId.value = ''
      currentModel.value = ''
    }
  }

  /** 从后端拉取 Provider 列表；失败则用 mock 兜底（浏览器独立开发）。 */
  async function fetchProviders() {
    loading.value = true
    lastError.value = null
    try {
      const data = await apiGet<{ providers: ProviderConfig[]; active: string }>('/api/providers')
      usingMock.value = false
      applyList(data.providers ?? [], data.active ?? '')
    } catch (e) {
      usingMock.value = true
      lastError.value = String(e)
      applyList(MOCK_PROVIDERS, 'openai')
    } finally {
      loading.value = false
    }
  }

  function selectModel(providerId: string, model: string) {
    currentProviderId.value = providerId; currentModel.value = model
    localStorage.setItem('coomi.providerId', providerId)
    localStorage.setItem('coomi.model', model)
  }
  /** 只同步顶栏的模型显示，不写 localStorage（切换会话时用，避免污染全局默认选择）。 */
  function syncDisplayModel(providerId: string, model: string) {
    currentProviderId.value = providerId
    currentModel.value = model
  }
  function setPermissionMode(mode: PermissionMode) {
    permissionMode.value = mode
    localStorage.setItem('coomi.permissionMode', mode)
  }
  function setReasoningEffort(effort: ReasoningEffort) {
    reasoningEffort.value = effort
    localStorage.setItem('coomi.reasoningEffort', effort)
  }
  function setMaxToolRounds(rounds: number) {
    maxToolRounds.value = [192, 256, 512].includes(rounds) ? rounds : 192
    localStorage.setItem('coomi.maxToolRounds', String(maxToolRounds.value))
  }

  function cacheConnectionSettings(value: ConnectionSettings) {
    connectionSettings.value = { ...DEFAULT_CONNECTION_SETTINGS, ...value }
    localStorage.setItem('coomi.providerRetryCount', String(value.providerRetryCount))
    localStorage.setItem('coomi.wsRetryCount', String(value.wsRetryCount))
    localStorage.setItem('coomi.reconnectInitialDelayMs', String(value.reconnectInitialDelayMs))
    localStorage.setItem('coomi.reconnectMaxDelayMs', String(value.reconnectMaxDelayMs))
    localStorage.setItem('coomi.maxConcurrentTasks', String(value.maxConcurrentTasks))
    localStorage.setItem('coomi.autoCompactPercent', String(connectionSettings.value.autoCompactPercent))
  }

  async function fetchConnectionSettings(): Promise<boolean> {
    try {
      const value = await apiGet<ConnectionSettings>('/api/settings/connection')
      cacheConnectionSettings(value)
      return true
    } catch {
      return false
    }
  }

  async function saveConnectionSettings(value: ConnectionSettings): Promise<boolean> {
    const normalized: ConnectionSettings = {
      providerRetryCount: Math.trunc(value.providerRetryCount),
      wsRetryCount: Math.trunc(value.wsRetryCount),
      reconnectInitialDelayMs: Math.trunc(value.reconnectInitialDelayMs),
      reconnectMaxDelayMs: Math.trunc(value.reconnectMaxDelayMs),
      maxConcurrentTasks: Math.trunc(value.maxConcurrentTasks),
      autoCompactPercent: Math.trunc(value.autoCompactPercent),
    }
    if (normalized.providerRetryCount < 0 || normalized.providerRetryCount > 10
      || normalized.wsRetryCount < 0 || normalized.wsRetryCount > 100
      || normalized.reconnectInitialDelayMs < 500 || normalized.reconnectInitialDelayMs > 60_000
      || normalized.reconnectMaxDelayMs < 1_000 || normalized.reconnectMaxDelayMs > 120_000
      || !Number.isFinite(normalized.autoCompactPercent) || normalized.autoCompactPercent < 10 || normalized.autoCompactPercent > 95
      || normalized.maxConcurrentTasks < 1 || normalized.maxConcurrentTasks > 20
      || normalized.reconnectMaxDelayMs < normalized.reconnectInitialDelayMs) return false
    try {
      const saved = await apiSend<ConnectionSettings>('/api/settings/connection', 'PUT', normalized)
      cacheConnectionSettings(saved)
      return true
    } catch {
      return false
    }
  }

  /**
   * 三档主题。应用后：
   * - 写入 <html data-theme>（前端样式即时切换）；
   * - Android WebView 内通知原生（CoomiAndroid.setThemeMode），原生据此改状态栏
   *   颜色并重新注入 data-theme；桌面浏览器直接由 applyTheme 生效。
   */
  function setThemeMode(mode: ThemeMode) {
    if (document.documentElement.dataset.customAppearance === 'true') return
    themeMode.value = mode
    localStorage.setItem('coomi.themeMode', mode)
    applyTheme(mode)
    const bridge = (window as any).CoomiAndroid
    if (bridge && typeof bridge.setThemeMode === 'function') {
      try { bridge.setThemeMode(mode) } catch { /* 忽略桥异常 */ }
    }
  }
  function cyclePermissionMode(): PermissionMode {
    const order: PermissionMode[] = ['ask', 'auto', 'full', 'minimal']
    const idx = order.indexOf(permissionMode.value)
    permissionMode.value = order[(idx + 1) % order.length]
    return permissionMode.value
  }
  function setProductionMode(value: 'normal' | 'overload' | 'berserk') {
    productionMode.value = value
    localStorage.setItem('coomi.productionMode', value)
    void apiSend('/api/settings/production-mode', 'POST', { mode: value }).catch(() => undefined)
  }
  const berserkModel = ref(localStorage.getItem('coomi.berserkModel') ?? '')
  function setBerserkModel(model: string) {
    berserkModel.value = model
    localStorage.setItem('coomi.berserkModel', model)
    void apiSend('/api/settings/berserk-model', 'POST', { model }).catch(() => undefined)
  }
  function setDefaultPermissionMode(mode: PermissionMode) {
    startupPermissionRead = true
    defaultPermissionMode.value = mode
    localStorage.setItem('coomi.defaultPermissionMode', mode)
    setPermissionMode(mode)
    void apiSend('/api/settings/permission', 'PUT', { defaultPermissionMode: mode })
      .catch((e) => { lastError.value = `默认权限未同步到引擎：${String(e)}` })
  }
  let startupPermissionRead = false
  async function syncStartupPermission() {
    if (startupPermissionRead) return
    try {
      const value = await apiGet<{ defaultPermissionMode: PermissionMode | null }>('/api/settings/permission')
      if (startupPermissionRead) return // 不覆盖获取过程中用户刚刚改动的设置
      if (value.defaultPermissionMode === null) {
        // 迁移旧版：引擎尚未记录启动默认值时，保留用户已选的本地默认值。
        await apiSend('/api/settings/permission', 'PUT', { defaultPermissionMode: defaultPermissionMode.value })
        // 迁移成功后立即更新 UI，避免下次启动前显示错误默认值
        if (!['ask', 'auto', 'full', 'minimal'].includes(defaultPermissionMode.value)) return
        localStorage.setItem('coomi.defaultPermissionMode', defaultPermissionMode.value)
        setPermissionMode(defaultPermissionMode.value)
        startupPermissionRead = true
        return
      }
      if (!['ask', 'auto', 'full', 'minimal'].includes(value.defaultPermissionMode)) return
      defaultPermissionMode.value = value.defaultPermissionMode
      localStorage.setItem('coomi.defaultPermissionMode', value.defaultPermissionMode)
      setPermissionMode(value.defaultPermissionMode)
      startupPermissionRead = true
    } catch { /* 引擎未就绪时保留本地默认，并在下次连接时重试 */ }
  }
  function setSendMorphAnimation(value: boolean) {
    sendMorphAnimation.value = value
    localStorage.setItem('coomi.sendMorphAnimation', value ? '1' : '0')
    // 打开液滴动画时，自动解除「关闭所有动画」总开关（互斥语义）。
    if (value && allAnimationsOff.value) {
      allAnimationsOff.value = false
      localStorage.removeItem('coomi.allAnimationsOff')
    }
  }
  /** 关闭全部动画：强制关闭液滴动画并注入全局无动画类名 / CSS 变量。 */
  function setAllAnimationsOff(value: boolean) {
    allAnimationsOff.value = value
    if (value) {
      localStorage.setItem('coomi.allAnimationsOff', '1')
      sendMorphAnimation.value = false
      localStorage.setItem('coomi.sendMorphAnimation', '0')
    } else {
      localStorage.removeItem('coomi.allAnimationsOff')
    }
  }
  function setMinimalUi(value: boolean) {
    minimalUi.value = value
    localStorage.setItem('coomi.minimalUi', value ? '1' : '0')
  }
  function togglePlanMode() { planMode.value = !planMode.value }

  /**
   * 全局会话记忆：关闭（默认）时 Coomi 无法读取任何历史会话文件；
   * 开启后它才能读取所有历史会话记录。历史会话列表始终可见，与本开关无关。
   * 引擎 settings.json 是权威值；localStorage 只是 UI 缓存，启动时以引擎为准。
   */
  const globalMemory = ref(localStorage.getItem('coomi.globalMemory') === '1')
  const digitalLifeEnabled = ref(localStorage.getItem('coomi.digitalLifeEnabled') === '1')
  /** 数字生命体「用于全局会话」：开启后所有对话都带生命体人格（引擎侧 settings.globalMode 为权威）。 */
  const lifeGlobalMode = ref(localStorage.getItem('coomi.lifeGlobalMode') === '1')

  function setLifeGlobalMode(enabled: boolean) {
    lifeGlobalMode.value = enabled
    localStorage.setItem('coomi.lifeGlobalMode', enabled ? '1' : '0')
  }

  function syncDigitalLifeEnabled() {
    const bridge = (window as any).CoomiAndroid
    if (bridge && typeof bridge.getDigitalLifeEnabled === 'function') {
      try { digitalLifeEnabled.value = !!bridge.getDigitalLifeEnabled() } catch { /* 使用本地缓存 */ }
    }
    localStorage.setItem('coomi.digitalLifeEnabled', digitalLifeEnabled.value ? '1' : '0')
  }

  async function fetchSubAgentSettings(): Promise<boolean> {
    if (usingMock.value) return true
    try {
      const data = await apiGet<SubAgentSettings>('/api/settings/subagents')
      subAgentSettings.value = {
        agents: (data.agents ?? []).map(agent => ({ ...agent })),
        fallbackId: data.fallbackId,
        maxAgents: Math.max(1, Math.min(30, data.maxAgents || 20)),
      }
      return true
    } catch (e) {
      lastError.value = String(e)
      return false
    }
  }

  async function saveSubAgentSettings(value: SubAgentSettings): Promise<boolean> {
    if (usingMock.value) {
      subAgentSettings.value = {
        agents: value.agents.map(agent => ({ ...agent })),
        fallbackId: value.fallbackId,
        maxAgents: value.maxAgents,
      }
      return true
    }
    try {
      const saved = await apiSend<SubAgentSettings>('/api/settings/subagents', 'PUT', value)
      subAgentSettings.value = {
        agents: (saved.agents ?? []).map(agent => ({ ...agent })),
        fallbackId: saved.fallbackId,
        maxAgents: Math.max(1, Math.min(30, saved.maxAgents || value.maxAgents)),
      }
      return true
    } catch (e) {
      lastError.value = String(e)
      return false
    }
  }
  async function validateAndSelectModel(providerId: string, model: string): Promise<boolean> {
    try {
      await apiSend(`/api/providers/${encodeURIComponent(providerId)}/select-model`, 'POST', { model })
      selectModel(providerId, model)
      activeId.value = providerId
      return true
    } catch (e) {
      lastError.value = String(e)
      return false
    }
  }

  function setDigitalLifeEnabled(enabled: boolean) {
    digitalLifeEnabled.value = enabled
    localStorage.setItem('coomi.digitalLifeEnabled', enabled ? '1' : '0')
    const bridge = (window as any).CoomiAndroid
    if (bridge && typeof bridge.setDigitalLifeEnabled === 'function') {
      try { bridge.setDigitalLifeEnabled(enabled) } catch { /* 本地状态仍可用 */ }
    }
  }
  /** 从引擎拉取权威值（应用启动时调用），覆盖本地缓存与开关显示。 */
  async function syncGlobalMemoryFromEngine() {
    try {
      const data = await apiGet<{ enabled: boolean }>('/api/runtime/global-memory')
      const enabled = !!data?.enabled
      globalMemory.value = enabled
      localStorage.setItem('coomi.globalMemory', enabled ? '1' : '0')
    } catch {
      /* 引擎未就绪：保持本地缓存，稍后用户操作开关时会再次同步 */
    }
  }
  async function toggleGlobalMemory() {
    const previous = globalMemory.value
    const next = !previous
    globalMemory.value = next
    localStorage.setItem('coomi.globalMemory', next ? '1' : '0')
    // 同步引擎侧：关闭时引擎屏蔽会话/配置目录的工具访问 + 系统提示加隐私禁令。
    // 失败必须回滚并提示，否则会出现「开关显示关、引擎实际开着」的脱节。
    try {
      await apiSend('/api/runtime/global-memory', 'POST', { enabled: next })
    } catch {
      globalMemory.value = previous
      localStorage.setItem('coomi.globalMemory', previous ? '1' : '0')
      throw new Error('同步引擎失败，开关已还原')
    }
  }

  /**
   * 定制身份提示词：用户设置的专属身份/定位指令，保存后注入系统提示词，
   * 让 AI 认知自己的身份与定位。引擎 settings.json 是权威值；
   * localStorage 只做 UI 缓存。
   */
  const customPrompt = ref(localStorage.getItem('coomi.customPrompt') ?? '')
  /** 从引擎拉取权威值（应用启动 / 进入设置页时调用）。 */
  async function fetchCustomPrompt() {
    try {
      const data = await apiGet<{ text: string }>('/api/runtime/custom-prompt')
      customPrompt.value = data?.text ?? ''
      localStorage.setItem('coomi.customPrompt', customPrompt.value)
      return true
    } catch {
      return false
    }
  }
  /** 保存定制提示词；空文本表示清除。成功返回 true。 */
  async function saveCustomPrompt(text: string): Promise<boolean> {
    try {
      const data = await apiSend<{ text: string }>('/api/runtime/custom-prompt', 'POST', { text })
      customPrompt.value = data?.text ?? text
      localStorage.setItem('coomi.customPrompt', customPrompt.value)
      return true
    } catch {
      return false
    }
  }

  /** 新增/更新 Provider。空 apiKey 表示沿用旧 key（后端语义）。 */
  async function upsertProvider(input: ProviderInput): Promise<boolean> {
    if (usingMock.value) {
      // 浏览器兜底：仅本地更新，不落盘
      const existing = providers.value.find(p => p.id === input.id)
      const apiKeyMasked = input.apiKey ? '****' + input.apiKey.slice(-4) : (existing?.apiKeyMasked ?? '')
      const hasKey = input.apiKey ? true : (existing?.hasKey ?? false)
      if (existing) {
        Object.assign(existing, {
          name: input.name, apiKeyMasked, hasKey, models: input.models,
          baseUrl: input.baseUrl, type: input.type, toolProtocol: input.toolProtocol,
          contextWindow: input.contextWindow, fastModel: input.fastModel,
          modelContextWindows: { ...(input.modelContextWindows ?? {}) },
          supportsWebSearch: input.supportsWebSearch, supportsVision: input.supportsVision,
          modelDescriptions: { ...(input.modelDescriptions ?? {}) }, modelParameters: { ...(input.modelParameters ?? {}) },
          capabilityOverrides: { ...(input.capabilityOverrides ?? {}) },
          model: input.models[0],
        })
      } else {
        providers.value.push({
          id: input.id, name: input.name, apiKeyMasked, hasKey, models: input.models,
          baseUrl: input.baseUrl, type: input.type, toolProtocol: input.toolProtocol,
          contextWindow: input.contextWindow, fastModel: input.fastModel,
          modelContextWindows: { ...(input.modelContextWindows ?? {}) },
          supportsWebSearch: input.supportsWebSearch, supportsVision: input.supportsVision,
          modelDescriptions: { ...(input.modelDescriptions ?? {}) }, modelParameters: { ...(input.modelParameters ?? {}) },
          capabilityOverrides: { ...(input.capabilityOverrides ?? {}) },
          model: input.models[0],
        })
      }
      if (input.activate) activeId.value = input.id
      return true
    }
    try {
      await apiSend('/api/providers', 'POST', {
        id: input.id,
        name: input.name,
        apiKey: input.apiKey,
        apiKeys: input.apiKeys ?? [],
        models: input.models,
        model: input.models[0],
        baseUrl: input.baseUrl,
        type: input.type,
        toolProtocol: input.toolProtocol,
        contextWindow: input.contextWindow,
        modelContextWindows: input.modelContextWindows,
        fastModel: input.fastModel,
        supportsWebSearch: input.supportsWebSearch,
        supportsVision: input.supportsVision,
        modelDescriptions: input.modelDescriptions,
        modelParameters: input.modelParameters,
        capabilityOverrides: input.capabilityOverrides,
        headers: input.headers,
        activate: input.activate,
      })
      await fetchProviders()
      return true
    } catch (e) {
      lastError.value = String(e)
      return false
    }
  }

  async function deleteProvider(id: string): Promise<boolean> {
    if (!id.trim()) return true
    if (usingMock.value) {
      const remaining = providers.value.filter(p => p.id !== id)
      applyList(remaining, activeId.value === id ? (remaining[0]?.id ?? '') : activeId.value)
      return true
    }
    try {
      await apiSend(`/api/providers/${encodeURIComponent(id)}`, 'DELETE')
      await fetchProviders()
      return true
    } catch (e) {
      lastError.value = String(e)
      return false
    }
  }

  async function activateProvider(id: string): Promise<boolean> {
    if (usingMock.value) {
      const provider = providers.value.find(item => item.id === id)
      if (!provider) return false
      activeId.value = id
      selectModel(id, provider.model || provider.models[0] || '')
      return true
    }
    try {
      await apiSend(`/api/providers/${encodeURIComponent(id)}/activate`, 'POST')
      await fetchProviders()
      const provider = providers.value.find(item => item.id === id)
      if (!provider) throw new Error('已激活的提供商未出现在配置列表中')
      const savedProvider = localStorage.getItem('coomi.providerId')
      const savedModel = localStorage.getItem('coomi.model')
      const model = savedProvider === id && provider.models.includes(savedModel ?? '')
        ? savedModel!
        : (provider.model || provider.models[0] || '')
      selectModel(id, model)
      return true
    } catch (e) {
      lastError.value = String(e)
      return false
    }
  }

  async function copyProvider(id: string): Promise<string | null> {
    try {
      const result = await apiSend<{ id: string }>(`/api/providers/${encodeURIComponent(id)}/copy`, 'POST')
      await fetchProviders()
      return result.id
    } catch (e) {
      lastError.value = String(e)
      return null
    }
  }

  async function revealProviderKey(id: string): Promise<string | null> {
    if (usingMock.value) return null
    try {
      const result = await apiSend<{ apiKey: string }>(`/api/providers/${encodeURIComponent(id)}/reveal`, 'POST')
      return result.apiKey
    } catch (e) {
      lastError.value = String(e)
      return null
    }
  }

  async function discoverModels(id: string, persist = false): Promise<{ models: string[]; contextWindows: Record<string, number> } | null> {
    if (usingMock.value) {
      const provider = providers.value.find(provider => provider.id === id)
      return { models: provider?.models ?? [], contextWindows: provider?.modelContextWindows ?? {} }
    }
    try {
      const result = await apiSend<{ models: string[]; contextWindows?: Record<string, number> }>(
        `/api/providers/${encodeURIComponent(id)}/discover-models`,
        'POST',
        { persist },
      )
      if (persist) await fetchProviders()
      return { models: result.models ?? [], contextWindows: result.contextWindows ?? {} }
    } catch (e) {
      lastError.value = String(e)
      return null
    }
  }

  return {
    ttsAutoRead, ttsRate, setTtsAutoRead, setTtsRate,
    mottoFont, setMottoFont,
    permissionMode, defaultPermissionMode, planMode, themeMode, reasoningEffort, maxToolRounds, connectionSettings, globalMemory, digitalLifeEnabled, lifeGlobalMode, setLifeGlobalMode, customPrompt, productionMode, setProductionMode, berserkModel, setBerserkModel, sendMorphAnimation, setSendMorphAnimation, allAnimationsOff, setAllAnimationsOff, minimalUi, setMinimalUi, providers, activeId, loading, usingMock, lastError, subAgentSettings,
    currentProviderId, currentModel, currentProvider, mergedProviders,
    fetchProviders, selectModel, syncDisplayModel, validateAndSelectModel, setPermissionMode, syncStartupPermission, setThemeMode, setReasoningEffort, setMaxToolRounds, fetchConnectionSettings, saveConnectionSettings, cyclePermissionMode, setDefaultPermissionMode, togglePlanMode,
    toggleGlobalMemory, syncGlobalMemoryFromEngine, setDigitalLifeEnabled, syncDigitalLifeEnabled, fetchCustomPrompt, saveCustomPrompt,
    upsertProvider, deleteProvider, activateProvider, copyProvider, revealProviderKey, discoverModels, fetchSubAgentSettings, saveSubAgentSettings,
  }
})

function readStoredInt(key: string, min: number, max: number, fallback: number): number {
  const value = Number(localStorage.getItem(key))
  return Number.isInteger(value) && value >= min && value <= max ? value : fallback
}
