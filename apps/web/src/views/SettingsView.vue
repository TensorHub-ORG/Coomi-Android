<script setup lang="ts">
/**
 * 设置。分组白卡 + 行的结构，选中态用蓝勾而不是描边 ——
 * 和抽屉、空态里的选中语言保持一致。
 */
import { computed, ref, onMounted, onBeforeUnmount, nextTick, watch } from 'vue'
import { useRouter } from 'vue-router'
import { goBack } from '@/bridge/navigation'
import { useConfigStore, DEFAULT_CONNECTION_SETTINGS, PERMISSION_MODES, REASONING_EFFORTS, type ConnectionSettings } from '@/stores/config'
import { useSessionStore } from '@/stores/session'
import { useSessionsStore } from '@/stores/sessions'
import { useConnectionStore } from '@/stores/connection'
import { authedFetch } from '@/bridge/http'
import { hasNativeTts } from '@/bridge/tts'
import { settingsTab, settingsScroll } from '@/stores/viewState'
import type { PermissionMode } from '@/protocol/commands'
import AutoCompactionSettings from '@/components/AutoCompactionSettings.vue'
import PageHead from '@/components/PageHead.vue'
import CoomiIcon from '@/components/CoomiIcon.vue'

const router = useRouter()
const config = useConfigStore()
const session = useSessionStore()
const sessions = useSessionsStore()
const connection = useConnectionStore()

const connectionDraft = ref<ConnectionSettings>({ ...config.connectionSettings })
const reconnectInitialSeconds = ref(config.connectionSettings.reconnectInitialDelayMs / 1000)
const activeTab = settingsTab
const body = ref<HTMLElement | null>(null)
watch(activeTab, async (tab, previous) => {
  if (body.value) settingsScroll.set(previous, body.value.scrollTop)
  await nextTick()
  if (body.value) body.value.scrollTop = settingsScroll.get(tab) ?? 0
})
onMounted(async () => {
  await nextTick()
  if (body.value) body.value.scrollTop = settingsScroll.get(activeTab.value) ?? 0
  if (router.currentRoute.value.hash === '#overload-model') document.getElementById('overload-model')?.scrollIntoView({ block:'start' })
})
onBeforeUnmount(() => {
  if (body.value) settingsScroll.set(activeTab.value, body.value.scrollTop)
})
const reconnectMaxSeconds = ref(config.connectionSettings.reconnectMaxDelayMs / 1000)
const connectionError = ref('')
const connectionSaved = ref(false)
/** 当前模型实际支持的推理档位；null 表示未知（保持全部可选项）。 */
const supportedReasoningEfforts = ref<string[] | null>(null)
const canDisableReasoning = ref(true)
const reasoningOptions = computed(() => REASONING_EFFORTS.filter(item => {
  if (item.value === 'auto') return true
  if (item.value === 'off') return canDisableReasoning.value
  return supportedReasoningEfforts.value === null || supportedReasoningEfforts.value.includes(item.value)
}))

watch([() => config.currentProviderId, () => config.currentModel], async ([providerId, model]) => {
  if (!providerId || !model) { supportedReasoningEfforts.value = null; canDisableReasoning.value = true; return }
  try {
    const res = await authedFetch(`/api/model-capabilities?provider_id=${encodeURIComponent(providerId)}&model=${encodeURIComponent(model)}`)
    if (!res.ok) throw new Error(String(res.status))
    const data = await res.json()
    const reasoning = data?.reasoning
    if (reasoning?.supported) {
      supportedReasoningEfforts.value = Array.isArray(reasoning.efforts) && reasoning.efforts.length ? reasoning.efforts : null
      canDisableReasoning.value = reasoning.can_disable !== false
    } else {
      supportedReasoningEfforts.value = []
      canDisableReasoning.value = false
    }
  } catch {
    // 能力探测失败时退回全量选项，不阻止用户选择。
    supportedReasoningEfforts.value = null
    canDisableReasoning.value = true
  }
}, { immediate: true })

watch(reasoningOptions, options => {
  if (!options.some(item => item.value === config.reasoningEffort)) session.setReasoningEffort('auto')
})
function resetConnectionSettings() {
  connectionDraft.value = { ...DEFAULT_CONNECTION_SETTINGS }
  reconnectInitialSeconds.value = DEFAULT_CONNECTION_SETTINGS.reconnectInitialDelayMs / 1000
  reconnectMaxSeconds.value = DEFAULT_CONNECTION_SETTINGS.reconnectMaxDelayMs / 1000
  void saveConnectionSettings()
}

async function saveConnectionSettings() {
  connectionError.value = ''
  connectionSaved.value = false
  if (!Number.isInteger(connectionDraft.value.providerRetryCount)
    || connectionDraft.value.providerRetryCount < 0 || connectionDraft.value.providerRetryCount > 10) {
    connectionError.value = '模型重试次数需为 0 - 10 的整数'
    return
  }
  if (!Number.isInteger(connectionDraft.value.maxConcurrentTasks)
    || connectionDraft.value.maxConcurrentTasks < 1 || connectionDraft.value.maxConcurrentTasks > 20) {
    connectionError.value = '会话并发数需为 1 - 20 的整数'
    return
  }
  if (!Number.isInteger(connectionDraft.value.wsRetryCount)
    || connectionDraft.value.wsRetryCount < 0 || connectionDraft.value.wsRetryCount > 100) {
    connectionError.value = 'WebSocket 重连次数需为 0 - 100 的整数'
    return
  }
  if (!Number.isFinite(reconnectInitialSeconds.value)
    || reconnectInitialSeconds.value < 0.5 || reconnectInitialSeconds.value > 60) {
    connectionError.value = '首次重连间隔需在 0.5 - 60 秒之间'
    return
  }
  if (!Number.isFinite(reconnectMaxSeconds.value)
    || reconnectMaxSeconds.value < 1 || reconnectMaxSeconds.value > 120) {
    connectionError.value = '最大重连间隔需在 1 - 120 秒之间'
    return
  }
  if (reconnectMaxSeconds.value < reconnectInitialSeconds.value) {
    connectionError.value = '最大重连间隔不能小于首次重连间隔'
    return
  }
  const value: ConnectionSettings = {
    ...connectionDraft.value,
    reconnectInitialDelayMs: Math.round(reconnectInitialSeconds.value * 1000),
    reconnectMaxDelayMs: Math.round(reconnectMaxSeconds.value * 1000),
  }
  if (await config.saveConnectionSettings(value)) {
    connectionDraft.value = { ...config.connectionSettings }
    connectionSaved.value = true
    session.reconnect()
  } else connectionError.value = '保存失败，请确认引擎已连接'
}

/** 全局记忆开关同步失败时的行内提示。 */
const gmError = ref('')
async function toggleGlobalMemory() {
  gmError.value = ''
  try {
    await config.toggleGlobalMemory()
  } catch (e) {
    gmError.value = e instanceof Error ? e.message : String(e)
  }
}

/** 匿名使用统计开关：仅上报 SKILL 安装/使用次数，不含任何内容。 */
const telemetryEnabled = ref(true)
const telemetryError = ref('')
async function toggleTelemetry() {
  telemetryError.value = ''
  try {
    const res = await authedFetch('/api/settings/telemetry', {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ enabled: !telemetryEnabled.value }),
    })
    if (!res.ok) throw new Error(`HTTP ${res.status}`)
    const data = await res.json()
    telemetryEnabled.value = data.enabled ?? !telemetryEnabled.value
  } catch (e) {
    telemetryError.value = `设置失败：${e instanceof Error ? e.message : String(e)}`
  }
}

/** 任务完成通知开关：原生侧 SharedPreferences（默认开）；桌面/浏览器无桥时隐藏整组。 */
const taskNotifySupported = ref(false)
const taskNotifyEnabled = ref(true)
function toggleTaskNotify() {
  const next = !taskNotifyEnabled.value
  taskNotifyEnabled.value = next
  try { window.CoomiAndroid?.setTaskNotifyEnabled?.(next) } catch { /* 本地状态仍可用 */ }
}

/** F8 语音陪伴：仅原生 TTS 桥存在时显示整组设置（桌面/浏览器自动隐藏）。 */
const nativeTts = ref(hasNativeTts())

const MODE_ICON: Record<PermissionMode, string> = { ask: 'shield', auto: 'bolt', full: 'plusCircle', minimal: 'terminal' }

/** provider × model 拍平成一维列表，省掉一层嵌套标题。 */
const modelRows = computed(() =>
  config.providers.flatMap(p =>
    p.models.map(m => ({ key: p.id + '::' + m, providerId: p.id, provider: p.name, model: m })),
  ),
)

function isCurrent(providerId: string, model: string): boolean {
  return config.currentProviderId === providerId && config.currentModel === model
}

/** 进入设置页时拉取定制提示词与统计开关状态（旧引擎无统计接口时保持默认）。 */
onMounted(async () => {
  void config.fetchCustomPrompt()
  if (await config.fetchConnectionSettings()) {
    connectionDraft.value = { ...config.connectionSettings }
    reconnectInitialSeconds.value = config.connectionSettings.reconnectInitialDelayMs / 1000
    reconnectMaxSeconds.value = config.connectionSettings.reconnectMaxDelayMs / 1000
  }
  try {
    const res = await authedFetch('/api/settings/telemetry')
    if (res.ok) {
      const data = await res.json()
      telemetryEnabled.value = data.enabled ?? true
    }
  } catch { /* 旧引擎进程：接口不存在，保持默认开启 */ }
  taskNotifySupported.value = typeof window.CoomiAndroid?.getTaskNotifyEnabled === 'function'
  if (taskNotifySupported.value) {
    try { taskNotifyEnabled.value = window.CoomiAndroid?.getTaskNotifyEnabled?.() ?? true } catch { /* 使用默认值 */ }
  }
})

</script>
<template>
  <div class="page">
    <PageHead title="设置" @back="goBack(router, '/')" />
      <div class="tabs" role="tablist" aria-label="设置分类">
        <button :class="{ on: activeTab === 'chat' }" @click="activeTab = 'chat'">对话</button>
        <button :class="{ on: activeTab === 'link' }" @click="activeTab = 'link'">连接</button>
        <button :class="{ on: activeTab === 'app' }" @click="activeTab = 'app'">应用</button>
      </div>
    <main ref="body" class="body">
<template v-if="activeTab === 'chat'">
      <p class="sec-label">权限模式</p>
      <div class="group">
        <button v-for="m in PERMISSION_MODES" :key="m.mode" class="row" @click="session.setPermissionMode(m.mode)">
          <span class="ri" :class="{ on: config.permissionMode === m.mode }">
            <CoomiIcon :name="MODE_ICON[m.mode]" :size="17" />
          </span>
          <span class="rt">
            <span class="rmain">{{ m.label }}</span>
            <span class="rsub">{{ m.desc }}</span>
          </span>
          <CoomiIcon v-if="config.permissionMode === m.mode" name="check" :size="17" class="tick" />
        </button>
      </div>

</template>

<template v-if="activeTab === 'chat'">
      <p class="option-note">当前默认：{{ PERMISSION_MODES.find(m => m.mode === config.defaultPermissionMode)?.label }}。点击下方选项可设置应用启动时的模式。</p>
      <div class="group compact-options permission-defaults">
        <button v-for="m in PERMISSION_MODES" :key="`default-${m.mode}`" class="option" :class="{ selected: config.defaultPermissionMode === m.mode }" @click="config.setDefaultPermissionMode(m.mode)">{{ m.label }}</button>
      </div>

      <p class="sec-label">对话模式</p>
      <div class="group">
        <button class="row" @click="session.togglePlanMode()">
          <span class="ri" :class="{ on: config.planMode }"><CoomiIcon name="target" :size="17" /></span>
          <span class="rt">
            <span class="rmain">计划模式</span>
            <span class="rsub">先给方案，你点头之后才动手</span>
          </span>
          <span class="sw" :class="{ on: config.planMode }" />
        </button>
        <button class="row" @click="toggleGlobalMemory">
          <span class="ri" :class="{ on: config.globalMemory }"><CoomiIcon name="clock" :size="17" /></span>
          <span class="rt">
            <span class="rmain">全局会话记忆</span>
            <span class="rsub" :class="{ err: !!gmError }">{{ gmError || (config.globalMemory ? '开启后 Coomi 可读取所有历史会话文件' : '全局会话记忆已关闭：Coomi 无法读取任何历史会话') }}</span>
          </span>
          <span class="sw" :class="{ on: config.globalMemory }" />
        </button>
      </div>

</template>

<template v-if="activeTab === 'chat'">
      <p class="sec-label">推理强度</p>
      <div class="group compact-options">
        <button v-for="item in reasoningOptions" :key="item.value" class="option" :class="{ selected: config.reasoningEffort === item.value }" @click="session.setReasoningEffort(item.value)">
          {{ item.label }}
        </button>
      </div>

</template>

<template v-if="activeTab === 'chat'">
      <p class="sec-label">工具调用上限</p>
      <div class="group compact-options rounds">
        <button v-for="rounds in [192, 256, 512]" :key="rounds" class="option" :class="{ selected: config.maxToolRounds === rounds }" @click="session.setMaxToolRounds(rounds)">
          {{ rounds }}
        </button>
      </div>
      <p class="option-note">默认 192，256 为进阶选项，512 为硬上限。</p>

</template>

<template v-if="activeTab === 'chat'"><AutoCompactionSettings /></template>

<template v-if="activeTab === 'link'">
      <p class="sec-label">连接、重试与并发</p>
      <div class="group numeric-settings">
        <label class="number-row">
          <span class="rt"><span class="rmain">模型重试次数</span><span class="rsub">瞬时网络或上游故障，0 表示禁用</span></span>
          <input v-model.number="connectionDraft.providerRetryCount" type="number" min="0" max="10" step="1" inputmode="numeric" aria-label="模型重试次数" />
        </label>
        <label class="number-row">
          <span class="rt"><span class="rmain">会话并发数</span><span class="rsub">同时运行的会话任务，重启引擎后生效</span></span>
          <input v-model.number="connectionDraft.maxConcurrentTasks" type="number" min="1" max="20" step="1" inputmode="numeric" aria-label="会话并发数" />
        </label>
        <label class="number-row">
          <span class="rt"><span class="rmain">WebSocket 重连次数</span><span class="rsub">界面与引擎断开后的尝试次数</span></span>
          <input v-model.number="connectionDraft.wsRetryCount" type="number" min="0" max="100" step="1" inputmode="numeric" aria-label="WebSocket 重连次数" />
        </label>
        <label class="number-row">
          <span class="rt"><span class="rmain">首次重连间隔</span><span class="rsub">0.5 - 60 秒</span></span>
          <input v-model.number="reconnectInitialSeconds" type="number" min="0.5" max="60" step="0.5" inputmode="decimal" aria-label="首次重连间隔（秒）" />
        </label>
        <label class="number-row">
          <span class="rt"><span class="rmain">最大重连间隔</span><span class="rsub">1 - 120 秒</span></span>
          <input v-model.number="reconnectMaxSeconds" type="number" min="1" max="120" step="0.5" inputmode="decimal" aria-label="最大重连间隔（秒）" />
        </label>
        <div class="settings-actions">
          <span class="save-state" :class="{ err: !!connectionError }">{{ connectionError || (connectionSaved ? '已保存并重新连接' : '') }}</span>
          <button class="text-action" type="button" @click="resetConnectionSettings">恢复默认</button>
          <button class="primary-action" type="button" @click="saveConnectionSettings">保存</button>
        </div>
      </div>

</template>

<template v-if="activeTab === 'chat'">
      <p class="sec-label">身份定位</p>
      <div class="group">
        <button class="row" @click="router.push('/persona')">
          <span class="ri" :class="{ on: config.customPrompt.trim() !== '' }"><CoomiIcon name="sparkle" :size="17" /></span>
          <span class="rt">
            <span class="rmain">定制身份定位</span>
            <span class="rsub">{{ config.customPrompt.trim() ? '已配置，置于系统提示词最前生效' : '未设置。让 AI 认知自己的身份与定位' }}</span>
          </span>
          <CoomiIcon name="chevronRight" :size="15" class="arw" />
        </button>
      </div>

</template>

<template v-if="activeTab === 'app'">
      <p class="sec-label">外观</p>
      <div class="group">
        <button class="row" @click="router.push('/appearance')">
          <span class="ri"><CoomiIcon name="sun" :size="17" /></span>
          <span class="rt"><span class="rmain">外观</span><span class="rsub">主题、颜色、背景与显示比例</span></span>
          <CoomiIcon name="chevronRight" :size="15" class="arw" />
        </button>
        <button class="row" @click="config.setAllAnimationsOff(!config.allAnimationsOff)">
          <span class="ri" :class="{ on: !config.allAnimationsOff }"><CoomiIcon name="play" :size="17" /></span>
          <span class="rt">
            <span class="rmain">关闭所有动画</span>
            <span class="rsub">关闭后禁用全部 CSS/GSAP/Morphicons/Web Animations，包括发送与工具展开动画</span>
          </span>
          <span class="sw" :class="{ on: config.allAnimationsOff }" />
        </button>
        <button class="row" @click="config.setSendMorphAnimation(!config.sendMorphAnimation)" :disabled="config.allAnimationsOff" :class="{ 'row-disabled': config.allAnimationsOff }">
          <span class="ri" :class="{ on: config.sendMorphAnimation && !config.allAnimationsOff }"><CoomiIcon name="sparkle" :size="17" /></span>
          <span class="rt">
            <span class="rmain">发送与工具动画</span>
            <span class="rsub">发送时显示按键光效，并控制工具卡展开与收回动画</span>
          </span>
          <span class="sw" :class="{ on: config.sendMorphAnimation && !config.allAnimationsOff }" />
        </button>
        <button class="row" @click="config.setMinimalUi(!config.minimalUi)">
          <span class="ri" :class="{ on: config.minimalUi }"><CoomiIcon name="terminal" :size="17" /></span>
          <span class="rt">
            <span class="rmain">极简界面模式</span>
            <span class="rsub">工具调用折叠成一小块方框，点开才看详情；文本与卡片更小更紧凑</span>
          </span>
          <span class="sw" :class="{ on: config.minimalUi }" />
        </button>
      </div>

</template>

<template v-if="activeTab === 'app'">
      <p class="sec-label">隐私</p>
      <div class="group">
        <button class="row" @click="router.push('/ux-program')">
          <span class="ri"><CoomiIcon name="shield" :size="17" /></span>
          <span class="rt"><span class="rmain">用户体验改进计划</span><span class="rsub">主动加入、随时退出，不在会话页自动邀请</span></span>
          <CoomiIcon name="chevronRight" :size="15" class="arw" />
        </button>
        <button class="row" @click="toggleTelemetry">
          <span class="ri" :class="{ on: telemetryEnabled }"><CoomiIcon name="shield" :size="17" /></span>
          <span class="rt">
            <span class="rmain">匿名使用统计</span>
            <span class="rsub" :class="{ err: !!telemetryError }">
              {{ telemetryError || (telemetryEnabled
                ? '仅上报 SKILL 安装与使用次数，不含任何对话内容，可随时关闭'
                : '已关闭：不再上报任何统计数据') }}
            </span>
          </span>
          <span class="sw" :class="{ on: telemetryEnabled }" />
        </button>
      </div>

      <p v-if="taskNotifySupported" class="sec-label">通知</p>
      <div v-if="taskNotifySupported" class="group">
        <button class="row" @click="toggleTaskNotify">
          <span class="ri" :class="{ on: taskNotifyEnabled }"><CoomiIcon name="bell" :size="17" /></span>
          <span class="rt">
            <span class="rmain">任务完成通知</span>
            <span class="rsub">{{ taskNotifyEnabled ? '后台任务完成时在通知栏提醒，点击可回前台查看结果' : '已关闭：任务完成时不再发送通知' }}</span>
          </span>
          <span class="sw" :class="{ on: taskNotifyEnabled }" />
        </button>
      </div>

      <p v-if="nativeTts" class="sec-label">语音朗读</p>
      <div v-if="nativeTts" class="group">
        <button class="row" @click="config.setTtsAutoRead(!config.ttsAutoRead)">
          <span class="ri" :class="{ on: config.ttsAutoRead }"><CoomiIcon name="volume2" :size="17" /></span>
          <span class="rt">
            <span class="rmain">AI 回复自动朗读</span>
            <span class="rsub">{{ config.ttsAutoRead ? 'AI 发送新消息后自动朗读内容' : '关闭自动朗读' }}</span>
          </span>
          <span class="sw" :class="{ on: config.ttsAutoRead }" />
        </button>
        <div class="row slider-row">
          <span class="ri"><CoomiIcon name="tachometer" :size="17" /></span>
          <span class="rt">
            <span class="rmain">朗读语速 {{ config.ttsRate.toFixed(1) }}x</span>
            <span class="rsub">拖动调整朗读速度（0.5 - 2.0 倍）</span>
          </span>
          <input
            type="range" min="0.5" max="2" step="0.1"
            :value="config.ttsRate"
            @input="config.setTtsRate(Number(($event.target as HTMLInputElement).value))"
            class="slider" aria-label="朗读语速"
          />
        </div>
      </div>

</template>

<template v-if="activeTab === 'link'">
      <p id="overload-model" class="sec-label">超载模型</p>
      <div class="group model-list">
        <p v-if="modelRows.length === 0" class="empty">还没有可用模型，先配置 Provider。</p>
        <button v-for="r in modelRows" :key="'bk-' + r.key" class="row" @click="config.setBerserkModel(r.providerId + ':' + r.model)">
          <span class="rt">
            <span class="rmain mono">{{ r.model }}</span>
            <span class="rsub">{{ r.provider }}</span>
          </span>
          <CoomiIcon v-if="config.berserkModel === r.providerId + ':' + r.model" name="check" :size="17" class="tick" />
        </button>
      </div>
      <p class="option-note">{{ config.berserkModel ? '超载模式将使用 ' + config.berserkModel + ' 检查并继续任务' : '未设置超载模型：切换超载模式时会提示先配置' }}</p>

      <p class="sec-label">模型</p>
      <div class="group model-list">
        <p v-if="modelRows.length === 0" class="empty">还没有可用模型，先到下面配置 Provider。</p>
        <button v-for="r in modelRows" :key="r.key" class="row" @click="session.selectModel(r.providerId, r.model)">
          <span class="rt">
            <span class="rmain mono">{{ r.model }}</span>
            <span class="rsub">{{ r.provider }}</span>
          </span>
          <CoomiIcon v-if="isCurrent(r.providerId, r.model)" name="check" :size="17" class="tick" />
        </button>
      </div>
</template>

<template v-if="activeTab === 'chat'">
      <p class="sec-label">会话与体验</p>
      <div class="group">
        <button class="row" @click="router.push('/prompts')">
          <span class="ri"><CoomiIcon name="pencil" :size="17" /></span>
          <span class="rt"><span class="rmain">提示词库</span><span class="rsub">分类管理、搜索编辑与一键填入</span></span>
          <CoomiIcon name="chevronRight" :size="15" class="arw" />
        </button>
        <button class="row" @click="router.push('/notes')">
          <span class="ri"><CoomiIcon name="notebook" :size="17" /></span>
          <span class="rt"><span class="rmain">笔记</span></span>
          <CoomiIcon name="chevronRight" :size="15" class="arw" />
        </button>
        <button class="row" @click="router.push('/quick-commands')">
          <span class="ri"><CoomiIcon name="bolt" :size="17" /></span>
          <span class="rt"><span class="rmain">新会话快捷指令</span><span class="rsub">四条指令、多套方案与恢复默认</span></span>
          <CoomiIcon name="chevronRight" :size="15" class="arw" />
        </button>
        <button class="row" @click="router.push('/sessions')">
          <span class="ri"><CoomiIcon name="chat" :size="17" /></span>
          <span class="rt"><span class="rmain">会话历史</span></span>
          <span class="rside">{{ sessions.metas.length }}</span>
          <CoomiIcon name="chevronRight" :size="15" class="arw" />
        </button>
      </div>

</template>

<template v-if="activeTab === 'app'">
      <p class="sec-label">版本管理</p>
      <div class="group">
        <button class="row" @click="router.push('/git')">
          <span class="ri"><CoomiIcon name="git" :size="17" /></span>
          <span class="rt"><span class="rmain">Git 面板</span><span class="rsub">改动、分支、历史、Stash 与远端同步</span></span>
          <CoomiIcon name="chevronRight" :size="15" class="arw" />
        </button>
        <button class="row" @click="router.push('/restore')">
          <span class="ri"><CoomiIcon name="clock" :size="17" /></span>
          <span class="rt"><span class="rmain">一键还原</span><span class="rsub">按快照预览差异并回退工作区</span></span>
          <CoomiIcon name="chevronRight" :size="15" class="arw" />
        </button>
        <button class="row" @click="router.push('/ops')">
          <span class="ri"><CoomiIcon name="wrench" :size="17" /></span>
          <span class="rt"><span class="rmain">运维诊断</span><span class="rsub">网络连通、存储分析与凭据管理</span></span>
          <CoomiIcon name="chevronRight" :size="15" class="arw" />
        </button>
        <button class="row" @click="router.push('/data')">
          <span class="ri"><CoomiIcon name="grep" :size="17" /></span>
          <span class="rt"><span class="rmain">数据工具</span><span class="rsub">贡献统计、用量与会话导出搜索</span></span>
          <CoomiIcon name="chevronRight" :size="15" class="arw" />
        </button>
      </div>

      </template>
<div class="foot">
        <span class="conn" :class="{ on: connection.isOpen }"><i />{{ connection.label }}</span>
        <span class="sid">{{ session.sessionId }}</span>
      </div>

    </main>
  </div>
</template>
<style scoped>
.page { display: flex; flex-direction: column; height: 100%; background: var(--page); }
.body { flex: 1; overflow-y: auto; padding: 14px 12px calc(var(--safe-bottom) + 24px); }
.tabs { display: flex; gap: 6px; margin: 10px 0 4px; overflow-x: auto; scrollbar-width: none; flex-wrap: nowrap; }
.tabs::-webkit-scrollbar { display: none; }
.tabs button { flex: 1 0 auto; min-height: 36px; border: 1px solid var(--border); border-radius: var(--r-pill); background: var(--bg); color: var(--text-2); font-size: 12.5px; }
.tabs button.on { background: var(--blue-soft); border-color: var(--blue-border, var(--blue)); color: var(--blue); font-weight: 650; }
.sec-label { margin: 16px 0 0; }
.sec-label:first-child { margin-top: 2px; }

.group { border-radius: var(--r-card); background: var(--bg); box-shadow: var(--shadow-1); overflow: hidden; }
.model-list {
  max-height: min(42vh, 360px); overflow-y: auto;
  overscroll-behavior: contain; -webkit-overflow-scrolling: touch;
}
.compact-options { display: grid; grid-template-columns: repeat(5, minmax(0, 1fr)); padding: 5px; gap: 4px; }
.compact-options.rounds { grid-template-columns: repeat(3, minmax(0, 1fr)); }
.option { min-width: 0; height: 38px; padding: 0 4px; border-radius: 6px; background: transparent; color: var(--text-2); font-size: 13px; }
.option.selected { background: var(--blue-soft); color: var(--blue); font-weight: 650; }
.option-note { margin: 6px 4px 0; font-size: 11.5px; color: var(--text-3); }
.option-note.ok { color: var(--ok); }
.row {
  display: flex; align-items: center; gap: 11px;
  width: 100%; min-height: 56px; padding: 11px 13px;
  text-align: left; background: var(--bg);
}
.row + .row { border-top: 1px solid var(--border); }
:root:not([data-all-animations-off]) .row:active { background: var(--fill); }
.row-disabled { opacity: .45; pointer-events: none; }
.row-disabled:active { background: var(--bg) !important; }
.theme-options.disabled { opacity: .42; }
.theme-options .row:disabled { color: inherit; cursor: default; }
.theme-options .row:disabled:active { background: var(--bg); }
.number-row { display: flex; align-items: center; gap: 12px; min-height: 64px; padding: 10px 13px; }
.number-row + .number-row { border-top: 1px solid var(--border); }
.number-row input { width: 92px; height: 38px; padding: 0 8px; border: 1px solid var(--border-strong); border-radius: 6px; background: var(--page); color: var(--text); text-align: right; font-variant-numeric: tabular-nums; }
.settings-actions { display: flex; align-items: center; gap: 8px; min-height: 50px; padding: 8px 12px; border-top: 1px solid var(--border); }
.save-state { flex: 1; min-width: 0; font-size: 12px; color: var(--ok); }
.save-state.err { color: var(--danger, #d43d2e); }
.text-action, .primary-action { height: 34px; padding: 0 11px; border-radius: 6px; font-size: 13px; }
.text-action { color: var(--text-2); background: var(--fill); }
.primary-action { color: #fff; background: var(--blue); }

.ri {
  display: grid; place-items: center; flex-shrink: 0;
  width: 32px; height: 32px; border-radius: 9px;
  background: var(--fill-strong); color: var(--text-2);
  transition: background .16s, color .16s;
}
.ri.on { background: var(--blue-soft); color: var(--blue); }

.rt { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 1px; }
.rmain { font-size: 14.5px; font-weight: 550; color: var(--text); }
.rmain.mono { font-family: var(--font-mono); font-size: 13.2px; word-break: break-all; }
.rsub { font-size: 12.2px; line-height: 1.5; color: var(--text-3); }
.rsub.err { color: var(--danger, #d43d2e); }
.rside { flex-shrink: 0; font-size: 13px; color: var(--text-3); font-variant-numeric: tabular-nums; }
.tick { flex-shrink: 0; color: var(--blue); }
.arw { flex-shrink: 0; color: var(--text-3); }
.empty { padding: 15px 14px; font-size: 13px; line-height: 1.6; color: var(--text-3); }

.sw { position: relative; flex-shrink: 0;
  width: 44px; height: 26px; border-radius: 13px;
  background: var(--border-strong); transition: background .2s;
}
.sw::after {
  content: ''; position: absolute; top: 2.5px; left: 2.5px;
  width: 21px; height: 21px; border-radius: 50%;
  background: #fff; box-shadow: var(--shadow-1); transition: transform .2s;
}
.sw.on { background: var(--blue); }
.sw.on::after { transform: translateX(18px); }

/* F8 语音陪伴：朗读语速滑杆 */
.slider-row { min-height: 64px; }
.slider-row .rt { flex: 1; }
.slider {
  flex-shrink: 0; width: 118px; height: 26px;
  appearance: none; -webkit-appearance: none; background: transparent;
  cursor: pointer; touch-action: none;
}
.slider::-webkit-slider-runnable-track {
  height: 5px; border-radius: 3px;
  background: var(--border-strong);
}
.slider::-webkit-slider-thumb {
  appearance: none; -webkit-appearance: none;
  width: 20px; height: 20px; margin-top: -7.5px; border-radius: 50%;
  background: var(--blue); box-shadow: var(--shadow-1);
  border: none; transition: transform .12s;
}
.slider::-webkit-slider-thumb:active { transform: scale(1.12); }
.slider::-moz-range-track {
  height: 5px; border-radius: 3px; background: var(--border-strong);
}
.slider::-moz-range-thumb {
  width: 20px; height: 20px; border-radius: 50%;
  background: var(--blue); box-shadow: var(--shadow-1); border: none;
}

.foot { display: flex; align-items: center; justify-content: center; gap: 9px; margin-top: 22px; }
.conn { display: inline-flex; align-items: center; gap: 6px; font-size: 12.5px; color: var(--text-3); }
.conn i { width: 6px; height: 6px; border-radius: 50%; background: var(--text-3); }
.conn.on i { background: var(--ok); }
.sid { max-width: 45vw; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-family: var(--font-mono); font-size: 11.5px; color: var(--text-3); }
</style>
