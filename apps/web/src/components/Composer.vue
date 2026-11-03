<script setup lang="ts">
/**
 * 输入区。
 * DeepSeek 的输入框是「一整块大圆角卡片」：文本在上，模式开关和发送在下一行。
 * 这里的两个 chip 都对应真实协议能力（enter/exit_plan_mode、set_permission_mode），
 * ⊕ 展开指令面板：Android SAF 文件导入 + 可滚动的斜杠指令列表。
 */
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { PERMISSION_MODES, REASONING_EFFORTS, useConfigStore } from '@/stores/config'
import { completePendingFileTransfer, useSessionStore } from '@/stores/session'
import { settingsTab } from '@/stores/viewState'
import { useRouter } from 'vue-router'
import { pushTrace, setControlModeActive } from '@/bridge/controlFloat'
import { MorphIcon } from 'morphicons/vue'
import { gsap, prefersReducedMotion } from '@/composables/useGsap'
import { Menu, ChevronUp, Plus, X } from 'lucide'
import CoomiIcon from './CoomiIcon.vue'
import AttachmentStrip from './AttachmentStrip.vue'
import { normalizeAttachments, type ChatAttachment } from '@/utils/attachments'
import { registerOverlay, unregisterOverlay } from '@/bridge/overlayStack'

const emit = defineEmits<{ (e: 'modes-open', open: boolean): void }>()
const session = useSessionStore()
const config = useConfigStore()
const router = useRouter()

/** 斜杠指令：点击后先确认，再按命令类型执行或填入输入框。 */
const SLASH_COMMANDS = [
  { name: '/loop', desc: '循环执行直到完成' },
  { name: '/plan', desc: '进入计划模式' },
  { name: '/mcp', desc: '管理 MCP 服务器' },
  { name: '/skills', desc: '查看可用技能' },
  { name: '/memory', desc: '查看持久记忆（不含记忆库）' },
  { name: '/workflows', desc: '打开持久化工作流' },
  { name: '/compact', desc: '压缩当前上下文（需确认）' },
]

const text = ref('')
const textarea = ref<HTMLTextAreaElement | null>(null)
const controlText = ref('')
const controlInput = ref<HTMLTextAreaElement | null>(null)
function toggleModelForControl() { /* 复用顶栏模型选择 */ (window as any).dispatchEvent(new Event('coomi:open-model-picker')) }
function sendControl() {
  const t = controlText.value.trim()
  if (!t) return
  controlText.value = ''
  if (!accessibilityReady.value && !overlayReady.value) {
    controlThinking.value = '还没有屏幕操控权限，请先点上方「开启无障碍」'
    return
  }
  controlThinking.value = `已发送：${t}`
  pushFloat('控制模式 · 正在发送', t)
  session.sendMessage(`控制模式任务：先用 ui_automation read_screen 查看当前手机界面，再按控件文字或返回坐标操作，每次操作后读取界面验证。\n\n${t}`)
}
const sendButton = ref<HTMLButtonElement | null>(null)
const sendRipples = ref<number[]>([])
let rippleSequence = 0
const quickOpen = ref(false)
watch(quickOpen, open => {
  if (open) registerOverlay('composer-quick', () => { quickOpen.value = false })
  else unregisterOverlay('composer-quick')
})
const transferText = ref('')
const transferProgress = ref(0)
const attachments = ref<ChatAttachment[]>([])
const hasNative = typeof window !== 'undefined' && !!window.CoomiAndroid

const canSend = computed(() => text.value.trim().length > 0 || attachments.value.length > 0)
const isJumpIn = computed(() => session.isBusy && canSend.value)
const showStop = computed(() => session.isBusy && !canSend.value)

/**
 * 底部模式按钮溢出折叠。
 *
 * 其他机型上「计划 / 权限 / 超载 / 控制」一排按钮可能超宽 → 折叠成三横杠，
 * 点击后向上展开（Morphicons 把三横杠变形成向上箭头 + 面板上升）。
 */
const barOpen = ref(false)
const barIcon = ref(Menu)
const quickIcon = computed(() => quickOpen.value ? X : Plus)
function toggleBar() {
  barOpen.value = !barOpen.value
  barIcon.value = barOpen.value ? ChevronUp : Menu
}

/** 模式面板：原大小按钮从模式键位置模糊分裂展开（stagger），收回时聚拢消失。 */
const modePopEl = ref<HTMLElement | null>(null)
watch(barOpen, (open) => {
  emit('modes-open', open)
  if (open) registerOverlay('composer-modes', () => { barOpen.value=false; barIcon.value=Menu })
  else unregisterOverlay('composer-modes')
  const panel = modePopEl.value
  if (!panel) return
  const buttons = Array.from(panel.querySelectorAll<HTMLElement>('.pill'))
  if (prefersReducedMotion() || !config.sendMorphAnimation || config.allAnimationsOff) return
  if (open) {
    gsap.killTweensOf(buttons)
    gsap.fromTo(buttons,
      { opacity: 0, scale: 0.6, filter: 'blur(6px)', y: 10 },
      {
        opacity: 1, scale: 1, filter: 'blur(0px)', y: 0,
        duration: 0.32, ease: 'power2.out', stagger: 0.045,
      },
    )
  } else {
    gsap.killTweensOf(buttons)
    gsap.to(buttons, {
      opacity: 0, scale: 0.6, filter: 'blur(6px)', y: 8,
      duration: 0.2, ease: 'power2.in', stagger: 0.025,
    })
  }
}, { flush: 'post' })
const modeLabel = computed(() => PERMISSION_MODES.find(m => m.mode === config.permissionMode)?.label ?? '')
const providerReady = computed(() => config.providers.some(provider => (
  provider.id === config.activeId
  && provider.models.length > 0
  && Boolean(provider.baseUrl)
)))

function autoGrow() {
  const el = textarea.value
  if (!el) return
  const container = el.closest<HTMLElement>('.composer-content')
  const previousTop = container?.scrollTop ?? 0
  const atBottom = !!container && container.scrollHeight - container.clientHeight - previousTop < 4
  el.style.height = 'auto'
  const scrollHeight = el.scrollHeight
  el.style.height = scrollHeight + 'px'
  if (container) container.scrollTop = atBottom ? container.scrollHeight : previousTop
}

function onTextInput() {
  autoGrow()
  // Restore after Vue and WebView finish the input layout; keep the edited line
  // visible without moving a user who is editing earlier text to the end.
  void nextTick(() => {
    autoGrow()
    const el = textarea.value
    const container = el?.closest<HTMLElement>('.composer-content')
    if (el && container && el.selectionEnd === el.value.length) container.scrollTop = container.scrollHeight
  })
}

async function submit() {
  if (!canSend.value) return
  if (!providerReady.value) {
    await config.fetchProviders()
    if (!providerReady.value) {
      await router.push('/providers')
      return
    }
  }
  const visibleText = text.value.trim()
  const messageId = session.sendMessage(visibleText, visibleText, false, [...attachments.value])
  if (messageId && config.sendMorphAnimation && !config.allAnimationsOff && !prefersReducedMotion()) {
    sendRipples.value = [++rippleSequence]
  }
  text.value = ''
  attachments.value = []
  await nextTick()
  autoGrow()
}

/** 主按钮：空着且在忙 = 停止，其余 = 发送 / 插队。 */
function tapPrimary() {
  if (showStop.value) session.cancel()
  else submit()
}

function onKeydown(e: KeyboardEvent) {
  // Enter 默认换行（需求：换行键就换行）；Ctrl/Cmd+Enter 仍可快捷发送。
  if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) { e.preventDefault(); submit() }
}

const modeNotice = ref('')
// 推理强度粒子数量（1.4.8 的 effort 特效，档位越高粒子越多）。
const EFFORT_PARTICLES = { off: 0, auto: 3, low: 2, medium: 4, high: 6, xhigh: 8 } as const
let modeNoticeTimer: ReturnType<typeof setTimeout> | null = null
function flashModeNotice(text: string) {
  modeNotice.value = text
  if (modeNoticeTimer) clearTimeout(modeNoticeTimer)
  modeNoticeTimer = setTimeout(() => { modeNotice.value = ''; modeNoticeTimer = null }, 8000)
}
function openOverloadSettings() {
  modeNotice.value = ''; barOpen.value = false
  settingsTab.value = 'link'
  void router.push('/settings#overload-model')
}
function cycleProductionMode() {
  if (config.productionMode !== 'normal') { session.setProductionMode('normal'); return }
  if (!config.berserkModel) {
    barOpen.value = false; barIcon.value = Menu
    flashModeNotice('选择超载模型，让 AI 自动检查成果并继续完成任务。')
    return
  }
  session.setProductionMode('berserk')
}
function toggleProductionMode() { cycleProductionMode() }

  function cycleMode() { session.setPermissionMode(config.cyclePermissionMode()) }

async function insert(t: string) {
  text.value = text.value.trim() ? text.value.replace(/\s+$/, '') + '\n' + t : t
  quickOpen.value = false
  await nextTick()
  autoGrow()
  textarea.value?.focus()
}

/** 斜杠指令：直接替换输入框内容，可继续编辑。 */
async function insertSlash(cmd: string) {
  text.value = cmd
  quickOpen.value = false
  await nextTick()
  autoGrow()
  textarea.value?.focus()
}

/**
 * 「指令」列表里的每条命令：点击即执行，执行前统一弹一次确认。
 * 需要模型参与的命令把指令原文作为一轮请求发出，其余走本地动作（切模式 / 跳页面）。
 */
interface PendingCommand { name: string; desc: string; run: () => void | Promise<void> }
const commandConfirm = ref<PendingCommand | null>(null)
watch(commandConfirm, request => {
  if (request) registerOverlay('composer-command', () => { commandConfirm.value = null })
  else unregisterOverlay('composer-command')
})

const COMMAND_RUNNERS: Record<string, () => void | Promise<void>> = {
  '/loop': () => { session.sendMessage('/loop') },
  '/plan': () => { session.togglePlanMode() },
  '/mcp': () => { void router.push('/catalog') },
  '/skills': () => { void router.push('/catalog') },
  '/memory': () => { session.sendMessage('/memory') },
  '/workflows': () => { void router.push('/workflows') },
  '/compact': () => { session.sendMessage('/compact') },
}

function requestCommand(cmd: { name: string; desc: string }) {
  const run = COMMAND_RUNNERS[cmd.name]
  if (!run) { void insertSlash(cmd.name); return }
  quickOpen.value = false
  commandConfirm.value = { name: cmd.name, desc: cmd.desc, run }
}

async function confirmCommand() {
  const request = commandConfirm.value
  commandConfirm.value = null
  if (!request) return
  try { await request.run() } catch { /* 动作自身会给出提示 */ }
}

function toggleQuick() { quickOpen.value = !quickOpen.value }
const controlMode = ref(false)
const riskConfirm = ref(false)
watch(riskConfirm, open => {
  if (open) registerOverlay('control-risk', () => { riskConfirm.value = false })
  else unregisterOverlay('control-risk')
})
const controlThinking = ref('')
const controlChatPage = ref(false)
/** 控制模式的两个系统权限：无障碍（操控屏幕）+ 悬浮窗（桌面悬浮层）。 */
const accessibilityReady = ref(false)
const overlayReady = ref(false)
const floatRunning = ref(false)
let controlPageTimer: ReturnType<typeof setInterval> | null = null
let controlChatTimer: ReturnType<typeof setTimeout> | null = null

function nativeBridge(): any {
  return (window as any).CoomiAndroid
}

/** 同步三个系统状态：无障碍开关、悬浮窗权限、悬浮层是否在跑。 */
function refreshControlPermissions() {
  const bridge = nativeBridge()
  if (!bridge) return
  try {
    if (typeof bridge.isAccessibilityEnabled === 'function') accessibilityReady.value = !!bridge.isAccessibilityEnabled()
    if (typeof bridge.isOverlayGranted === 'function') overlayReady.value = !!bridge.isOverlayGranted()
    if (typeof bridge.isControlFloatRunning === 'function') {
      const now = !!bridge.isControlFloatRunning()
      if (floatRunning.value && !now && controlMode.value) { exitControlMode(); return }
      floatRunning.value = now
    }
  } catch { /* 桥不可用时保持原状态 */ }
}

/** 申请无障碍权限（系统设置页需用户手动开启，回到应用后轮询感知）。 */
function requestAccessibility() {
  const bridge = nativeBridge()
  if (bridge && typeof bridge.requestAccessibilityPermission === 'function') {
    bridge.requestAccessibilityPermission()
  }
}

/** 申请悬浮窗权限。 */
function requestOverlay() {
  const bridge = nativeBridge()
  if (bridge && typeof bridge.requestOverlayPermission === 'function') {
    bridge.requestOverlayPermission()
  }
}

/** 把当前动作与思考内容推到桌面悬浮层。 */
function pushFloat(title: string, body: string) {
  pushTrace(title, body)
}

function toggleControlMode() {
  if (controlMode.value) { exitControlMode(); return }
  // 风险提示用应用内浮层（WebView 中 window.confirm 会被吞）
  riskConfirm.value = true
}
function confirmControlMode() {
  riskConfirm.value = false
  controlMode.value = true
  controlThinking.value = ''
  // 打开转发开关：之后引擎侧的思考 / 工具调用会自动同步到悬浮层。
  setControlModeActive(true)
  refreshControlPermissions()
  // 无障碍是控制模式的主力：没有它就只能退回 Shizuku 的坐标点按。
  if (!accessibilityReady.value) requestAccessibility()
  if (!overlayReady.value) requestOverlay()
  window.dispatchEvent(new CustomEvent('coomi:control-mode', { detail: { enabled: true } }))
  // 待机检测：每 5s 尝试原生桥，检测进入微信/QQ 聊天页，同时刷新权限与悬浮层状态
  controlPageTimer = setInterval(() => {
    const bridge = nativeBridge()
    refreshControlPermissions()
    if (bridge && typeof bridge.controlForegroundApp === 'function') {
      try {
        const app = bridge.controlForegroundApp()
        controlChatPage.value = /weixin|wechat|qq\.com|tencent|mobileqq/i.test(String(app || ''))
      } catch { controlChatPage.value = false }
    }
  }, 5000)
}
function cancelControlMode() { riskConfirm.value = false }
function exitControlMode() {
  controlMode.value = false
  controlChatPage.value = false
  controlThinking.value = ''
  setControlModeActive(false)
  const bridge = nativeBridge()
  if (bridge && typeof bridge.stopControlFloat === 'function') {
    try { bridge.stopControlFloat() } catch { /* 忽略 */ }
  }
  floatRunning.value = false
  if (controlPageTimer) { clearInterval(controlPageTimer); controlPageTimer = null }
  if (controlChatTimer) { clearTimeout(controlChatTimer); controlChatTimer = null }
  window.dispatchEvent(new CustomEvent('coomi:control-mode', { detail: { enabled: false } }))
}
/** 待机检测到聊天页 → 用户点「对话」：把要回复的内容注入输入框并聚焦。 */
function startControlReply() {
  controlChatPage.value = false
  controlThinking.value = '检测到聊天页面，请在下方输入回复内容'
  pushFloat('控制模式 · 已进入聊天页', '可以输入回复内容，点发送即可填入并送出')
  nextTick(() => controlInput.value?.focus())
}

/**
 * 控制模式：无障碍就绪后自动把桌面悬浮层拉起来。
 * 悬浮层负责在用户切到微信/QQ 之后继续显示「正在调用哪个工具 / 思考到哪一步」，
 * 所以它必须独立于应用界面存在。
 */
watch([accessibilityReady, overlayReady, controlMode], ([accessible, overlay, active]) => {
  const bridge = nativeBridge()
  if (!active) return
  if (!overlay) return
  if (!bridge?.startControlFloat) return
  try { bridge.startControlFloat(); floatRunning.value=!!bridge.isControlFloatRunning?.() } catch { /* 忽略 */ }
  pushFloat('控制模式已就绪', accessible ? '无障碍已开启，可以直接操作屏幕' : '悬浮窗已开启，屏幕操控仍需要无障碍')
})

watch([() => session.sessionId, () => config.currentProviderId, () => config.currentModel], () => {
  if (controlMode.value) setControlModeActive(true)
})

/** 思考/动作变化时同步到悬浮层，让切到别的 App 后也能看到进度。 */
watch(controlThinking, value => {
  if (!controlMode.value) return
  pushFloat('控制模式 · 执行中', value || '')
})



function importFiles() { quickOpen.value = false; window.CoomiAndroid?.importFiles?.() }
function authorizeFolder() { quickOpen.value = false; window.CoomiAndroid?.authorizeFolder?.() }
function onTransferProgress(event: Event) {
  const detail = (event as CustomEvent<{ message?: string; progress?: number }>).detail ?? {}
  transferText.value = detail.message ?? '正在传输文件'
  transferProgress.value = detail.progress ?? 0
}
function onFilesImported(event: Event) {
  const detail = (event as CustomEvent<{ paths?: string[]; requestId?: string }>).detail ?? {}
  const paths = detail.paths ?? []
  if (detail.requestId && completePendingFileTransfer(detail.requestId, paths)) return
  transferText.value = paths.length ? `已导入 ${paths.length} 个文件` : '文件导入完成'
  transferProgress.value = 100
  if (detail.requestId) session.completeFileTransfer(detail.requestId, paths)
  else if (paths.length) attachments.value = normalizeAttachments([...attachments.value.map(item => item.path), ...paths])
  setTimeout(() => { transferText.value = ''; transferProgress.value = 0 }, 2600)
}
function onFileExported(event: Event) {
  const detail = (event as CustomEvent<{ requestId?: string; path?: string }>).detail ?? {}
  if (detail.requestId && completePendingFileTransfer(detail.requestId, detail.path ? [detail.path] : [])) return
  if (detail.requestId) session.completeFileTransfer(detail.requestId, detail.path ? [detail.path] : [])
}
function removeImportedFile(path: string) {
  attachments.value = attachments.value.filter(item => item.path !== path)
}
function onPrefillDraft(event: Event) {
  const detail = (event as CustomEvent<{ sessionId?: string; text?: string; attachments?: ChatAttachment[] }>).detail ?? {}
  if ((detail.sessionId && detail.sessionId !== session.sessionId) || typeof detail.text !== 'string') return
  text.value = detail.text
  attachments.value = normalizeAttachments((detail.attachments ?? []).map(item => item.path))
  void nextTick(autoGrow)
}
onMounted(() => {
  window.addEventListener('coomi:file-transfer-progress', onTransferProgress)
  window.addEventListener('coomi:files-imported', onFilesImported)
  window.addEventListener('coomi:file-exported', onFileExported)
  window.addEventListener('coomi:prefill-draft', onPrefillDraft)
  window.addEventListener('resize', autoGrow)
  loadDraft()
})
onBeforeUnmount(() => {
  unregisterOverlay('composer-modes')
  unregisterOverlay('composer-quick')
  unregisterOverlay('control-risk')
  window.removeEventListener('coomi:file-transfer-progress', onTransferProgress)
  window.removeEventListener('coomi:files-imported', onFilesImported)
  window.removeEventListener('coomi:file-exported', onFileExported)
  window.removeEventListener('coomi:prefill-draft', onPrefillDraft)
  window.removeEventListener('resize', autoGrow)
  saveDraft()
})

// ── 草稿按会话持久化：每个会话（含新对话）各自保留输入框内容 ──
const DRAFT_PREFIX = 'coomi.draft.'
const DRAFT_ATTACHMENTS_PREFIX = 'coomi.draft.attachments.'
let draftTimer: ReturnType<typeof setTimeout> | null = null

function draftKey(id: string) { return DRAFT_PREFIX + id }

function loadDraft() {
  let saved = ''
  try { saved = localStorage.getItem(draftKey(session.sessionId)) ?? '' } catch { /* ignore */ }
  text.value = saved
  try {
    const paths = JSON.parse(localStorage.getItem(DRAFT_ATTACHMENTS_PREFIX + session.sessionId) ?? '[]')
    attachments.value = Array.isArray(paths) ? normalizeAttachments(paths.map(String)) : []
  } catch { attachments.value = [] }
  void nextTick(autoGrow)
}

function saveDraft() {
  if (draftTimer) { clearTimeout(draftTimer); draftTimer = null }
  try { localStorage.setItem(draftKey(session.sessionId), text.value) } catch { /* ignore */ }
  try { localStorage.setItem(DRAFT_ATTACHMENTS_PREFIX + session.sessionId, JSON.stringify(attachments.value.map(item => item.path))) } catch { /* ignore */ }
}

// 切会话（含新建会话）时：先把旧会话的草稿存回【旧】key，再加载新会话草稿。
// 注意：watch 回调里 session.sessionId 已经是新值，保存必须用回调的 prev 参数，
// 否则旧内容会被写进新会话的 key，导致所有会话显示同一个草稿。
watch(() => session.sessionId, (next, prev) => {
  if (prev && prev !== next) {
    try { localStorage.setItem(draftKey(prev), text.value) } catch { /* ignore */ }
    try { localStorage.setItem(DRAFT_ATTACHMENTS_PREFIX + prev, JSON.stringify(attachments.value.map(item => item.path))) } catch { /* ignore */ }
  }
  loadDraft()
})
watch(text, () => {
  if (draftTimer) clearTimeout(draftTimer)
  draftTimer = setTimeout(saveDraft, 200)
})
watch(attachments, () => {
  if (draftTimer) clearTimeout(draftTimer)
  draftTimer = setTimeout(saveDraft, 200)
}, { deep: true })
</script>

<template>
  <div class="composer">
    <div v-if="session.pendingEdit" class="edit-banner">
      <span>正在编辑上一条消息，发送将覆盖该轮执行</span>
      <button @click="session.cancelEditMessage()">取消编辑</button>
    </div>
    <div v-if="transferText" class="transfer">
      <span>{{ transferText }}</span><progress :value="transferProgress" max="100" />
    </div>
    <div v-if="quickOpen || session.lifeStatsOpen" class="quick-scrim" @click="quickOpen = false; session.lifeStatsOpen = false" />
    <div v-if="quickOpen" class="quick">
      <p class="qhead reasoning-head">推理强度</p>
      <div class="reasoning-options"><button v-for="item in REASONING_EFFORTS" :key="item.value" :data-effort="item.value" :class="{ selected: config.reasoningEffort === item.value }" @click="session.setReasoningEffort(item.value)">
        <span v-if="config.reasoningEffort === item.value" class="effort-particles" aria-hidden="true">
          <i v-for="n in EFFORT_PARTICLES[item.value]" :key="n" class="effort-particle" />
        </span>
        <span class="effort-label">{{ item.label }}</span>
      </button></div>
      <p class="qhead">本地能力</p>
      <div class="local-actions">
        <button class="qchip file" @click="insertSlash('/local_model ')"><CoomiIcon name="sparkle" :size="15" />本地模型</button>
        <button class="qchip file" @click="insertSlash('/local_ocr ')"><CoomiIcon name="scan" :size="15" />本地 OCR</button>
        <button class="qchip file" @click="insertSlash('/local_tts ')"><CoomiIcon name="volume" :size="15" />语音合成</button>
        <button class="qchip file" @click="insertSlash('/ui_automation ')"><CoomiIcon name="cursor" :size="15" />UI 自动化</button>
      </div>
      <p class="qhead">指令</p>
      <div v-if="hasNative" class="file-actions">
        <button class="qchip file" @click="importFiles"><CoomiIcon name="fileRead" :size="15" />选择文件</button>
        <button class="qchip file" @click="authorizeFolder"><CoomiIcon name="folder" :size="15" />授权目录</button>
      </div>
      <div class="slash-list">
        <button v-for="c in SLASH_COMMANDS" :key="c.name" class="slash-item" @click="requestCommand(c)">
          <code>{{ c.name }}</code><span>{{ c.desc }}</span>
        </button>
      </div>
    </div>

    <!-- 指令执行确认浮层 -->
    <div v-if="commandConfirm" class="risk-mask" @click.self="commandConfirm = null">
      <div class="risk-dialog" role="alertdialog" aria-modal="true" aria-label="确认执行指令">
        <p class="cmd-title">执行 {{ commandConfirm.name }}？</p>
        <p class="risk-text">
          {{ commandConfirm.desc }}
          <template v-if="commandConfirm.name === '/compact'">会保留当前工作方向并压缩旧消息；这个操作会调用模型，完成后不能直接恢复原始上下文。</template>
          <template v-else-if="commandConfirm.name === '/memory'">会在本会话里输出持久记忆清单，不会改动任何记忆内容。</template>
          <template v-else-if="commandConfirm.name === '/loop'">会把该指令作为一轮请求发给模型。</template>
          <template v-else-if="commandConfirm.name === '/plan'">会切换计划模式：打开后先给方案，确认了再动手。</template>
          <template v-else>会打开对应页面，当前会话保持不动。</template>
        </p>
        <div class="risk-actions">
          <button class="btn ghost" @click="commandConfirm = null">取消</button>
          <button class="btn primary" @click="confirmCommand">确认执行</button>
        </div>
      </div>
    </div>

    <div class="field" :class="{ busy: session.isBusy }">
      <div v-if="session.lifeStatsOpen" class="life-stats-card">
        <header><span>生命状态</span><button aria-label="关闭" @click="session.lifeStatsOpen = false"><CoomiIcon name="close" :size="14" /></button></header>
        <div class="life-waveform" aria-label="数字生命动态状态波形">
          <svg viewBox="0 0 320 100" preserveAspectRatio="none" aria-hidden="true">
            <path class="wave wave-upper upper-back" d="M0 50C20 47 26 28 46 29C65 30 66 45 84 39C102 33 101 15 117 12C133 9 136 38 153 39C169 40 177 29 191 34C207 40 210 48 225 48C243 48 247 30 264 32C282 34 285 45 301 43C310 42 316 48 320 50V50H0Z" />
            <path class="wave wave-upper upper-main" d="M0 50C17 46 27 17 47 20C68 23 68 43 87 34C104 26 103 5 119 4C137 3 137 35 153 36C169 37 177 21 192 29C207 37 209 48 225 47C242 46 248 20 264 24C281 28 283 45 300 40C309 38 316 47 320 50V50H0Z" />
            <path class="wave wave-upper upper-front" d="M0 50C22 48 33 35 49 36C65 37 72 46 87 42C104 37 105 23 119 20C135 17 139 43 155 44C171 45 179 35 193 39C208 43 214 49 228 49C245 49 250 37 265 38C282 39 291 48 304 46C312 45 317 49 320 50V50H0Z" />
            <path class="wave wave-lower lower-back" d="M0 50C19 53 26 73 46 71C65 69 66 55 84 61C102 67 101 85 117 88C133 91 136 62 153 61C169 60 177 71 191 66C207 60 210 52 225 52C243 52 247 70 264 68C282 66 285 55 301 57C310 58 316 52 320 50V50H0Z" />
            <path class="wave wave-lower lower-main" d="M0 50C17 54 27 83 47 80C68 77 68 57 87 66C104 74 103 95 119 96C137 97 137 65 153 64C169 63 177 79 192 71C207 63 209 52 225 53C242 54 248 80 264 76C281 72 283 55 300 60C309 62 316 53 320 50V50H0Z" />
            <path class="wave wave-lower lower-front" d="M0 50C22 52 33 65 49 64C65 63 72 54 87 58C104 63 105 77 119 80C135 83 139 57 155 56C171 55 179 65 193 61C208 57 214 51 228 51C245 51 250 63 265 62C282 61 291 52 304 54C312 55 317 51 320 50V50H0Z" />
            <path class="wave-baseline" d="M0 50H320" />
          </svg>
        </div>
        <div class="life-stats-grid"><span>当前模式<strong>数字生命</strong></span><span>推理档位<strong>{{ REASONING_EFFORTS.find(i => i.value === config.reasoningEffort)?.label }}</strong></span><span>会话状态<strong>{{ session.isBusy ? '运行中' : '待命' }}</strong></span><span>动态流<strong>已连接</strong></span></div>
      </div>
      <div class="composer-content">
        <AttachmentStrip v-if="attachments.length" :items="attachments" removable @remove="removeImportedFile" />
        <textarea
          ref="textarea"
          v-model="text"
          class="input"
          rows="1"
          :placeholder="session.isBusy ? '插队补充指令…' : '给 Coomi 下达任务…'"
          @input="onTextInput"
          @keydown="onKeydown"
        />
      </div>

        <div class="bar">
          <!-- 三横杠：模式按钮竖向折叠，点击向上展开面板 -->
          <button class="pill bar-toggle" :class="{ on: barOpen }" @click="toggleBar" aria-label="更多模式" :aria-expanded="barOpen">
            <MorphIcon
              :icon="barIcon"
              :size="15"
              stroke-width="2"
              :spring="{ stiffness: 300, damping: 22 }"
              :reduced-motion="(config.sendMorphAnimation && !config.allAnimationsOff) ? 'never' : 'always'"
            />
            <span>模式</span>
          </button>

          <span class="spacer" />

          <button class="act" aria-label="快捷指令" @click="toggleQuick">
            <MorphIcon
              :icon="quickIcon"
              :size="21"
              stroke-width="2"
              :spring="{ stiffness: 320, damping: 24 }"
              :reduced-motion="(config.sendMorphAnimation && !config.allAnimationsOff) ? 'never' : 'always'"
            />
          </button>

          <button
            ref="sendButton"
            class="send"
            :class="{ jump: isJumpIn, stop: showStop }"
            :disabled="!canSend && !session.isBusy"
            :aria-label="showStop ? '停止' : isJumpIn ? '插队' : '发送'"
            @click="tapPrimary"
          >
            <CoomiIcon v-if="showStop" name="stop" :size="17" />
            <CoomiIcon v-else-if="isJumpIn" name="subtask" :size="18" />
            <CoomiIcon v-else name="arrowUp" :size="18" />
            <span v-for="ripple in sendRipples" :key="ripple" class="send-ripple" aria-hidden="true" @animationend.self="sendRipples = sendRipples.filter(value => value !== ripple)"><i /><i /></span>
          </button>
        </div>
                <Transition name="mode-pop">
          <div v-if="barOpen" ref="modePopEl" class="mode-pop" role="group" aria-label="模式选项">
            <button class="pill" :class="{ on: config.planMode }" @click="session.togglePlanMode()">
              <CoomiIcon name="target" :size="14" />
              <span>计划</span>
            </button>
            <button class="pill" :class="{ on: config.permissionMode === 'auto' || config.permissionMode === 'minimal', 'warn-on': config.permissionMode === 'full' }" @click="cycleMode()">
              <CoomiIcon name="shield" :size="14" />
              <span>{{ modeLabel }}</span>
            </button>
            <button class="pill production-pill" :class="{ on: config.productionMode !== 'normal', 'warn-on': config.productionMode === 'berserk' }" @click="toggleProductionMode()">
              <CoomiIcon name="target" :size="14" />
              <span>{{ config.productionMode !== 'normal' ? '超载' : '普通' }}</span>
            </button>
            <button class="pill control-pill" :class="{ on: controlMode }" @click="toggleControlMode()">
              <CoomiIcon name="cursor" :size="14" />
              <span>控制</span>
            </button>
          </div>
        </Transition>

    </div>

    <!-- 控制模式风险确认浮层 -->
    <div v-if="riskConfirm" class="risk-mask" @click.self="cancelControlMode">
      <div class="risk-dialog">
        <p class="risk-title">控制模式</p>
        <p class="risk-text">该功能风险极高，如出现问题软件作者不负责，且需要多模态模型与大量 token 消耗。确认后需要开启「无障碍」与「悬浮窗」权限：无障碍用于代替你点击、输入、发送，悬浮窗用于在你切换到别的 App 后继续显示执行进度（收起为窄条带，不挡屏幕）。</p>
        <div class="risk-actions">
          <button class="btn ghost" @click="cancelControlMode">取消</button>
          <button class="btn danger" @click="confirmControlMode">确认进入</button>
        </div>
      </div>
    </div>
    <!-- 控制模式浮窗：权限状态 + 指令输入 -->
    <div v-if="controlMode && !(hasNative && overlayReady)" class="control-float">
      <div class="control-bar">
        <span class="control-label">控制模式</span>
        <button class="exit-control" @click="exitControlMode">退出</button>
      </div>
      <div class="ctrl-perms">
        <button class="perm-chip" :class="{ ok: accessibilityReady }" @click="accessibilityReady ? undefined : requestAccessibility()">
          {{ accessibilityReady ? '✓ 无障碍已开启' : '开启无障碍' }}
        </button>
        <button class="perm-chip" :class="{ ok: overlayReady }" @click="overlayReady ? undefined : requestOverlay()">
          {{ overlayReady ? '✓ 悬浮窗已开启' : '开启悬浮窗' }}
        </button>
        <span v-if="floatRunning" class="perm-note">悬浮层已悬浮在桌面</span>
      </div>
      <textarea
        ref="controlInput"
        v-model="controlText"
        class="control-input"
        rows="2"
        placeholder="输入指令或回复内容…（会自动填入当前聊天框并发送）"
        @keydown.enter.prevent="sendControl"
      />
      <p v-if="controlThinking" class="control-thinking">{{ controlThinking }}</p>
      <button v-if="controlChatPage" class="control-chat-btn" type="button" @click="startControlReply">对话</button>
    </div>
    <div v-if="modeNotice" class="mode-notice" role="status"><div class="mode-notice-head"><CoomiIcon name="sparkle" :size="17"/><b>开启超载模式</b><button aria-label="关闭提示" @click="modeNotice=''"><CoomiIcon name="close" :size="15"/></button></div><p>{{ modeNotice }}</p><button class="mode-config" @click="openOverloadSettings">选择超载模型 <CoomiIcon name="arrowRight" :size="14"/></button></div>
  </div>
</template>

<style scoped>
.composer { position: relative; flex-shrink: 0; padding: 6px 10px calc(var(--safe-bottom) + 8px); background: var(--bg); }
.control-float {
  position: fixed; left: 10px; right: 10px; bottom: calc(var(--safe-bottom) + 78px); z-index: 80;
  padding: 10px 12px; border: 1px solid var(--blue-border); border-radius: var(--r-card);
  background: var(--bg); box-shadow: var(--shadow-2);
  display: flex; flex-direction: column; gap: 8px;
  animation: control-pop .2s cubic-bezier(.2,.9,.3,1.15) both;
}
@keyframes effort-particle-rise {
  0%,100% { opacity:0; transform:translate3d(0,3px,0) scale(.45); }
  22% { opacity:var(--particle-alpha); }
  55% { opacity:calc(var(--particle-alpha) * .85); transform:translate3d(var(--drift-x), var(--particle-rise), 0) scale(1); }
  80% { opacity:calc(var(--particle-alpha) * .4); }
  100% { opacity:0; transform:translate3d(calc(var(--drift-x) * 1.6), calc(var(--particle-rise) * 1.5), 0) scale(.55); }
}
@keyframes control-pop { from { opacity: 0; transform: translateY(10px) scale(.96); } to { opacity: 1; transform: none; } }
.risk-mask { position: fixed; inset: 0; z-index: 90; background: rgba(0,0,0,.45); display: flex; align-items: center; justify-content: center; padding: 28px; }
.risk-dialog { width: 100%; max-width: 320px; padding: 16px 15px; border-radius: var(--r-card); background: var(--bg); box-shadow: var(--shadow-2); display: flex; flex-direction: column; gap: 10px; }
.risk-title { margin: 0; font-size: 15px; font-weight: 750; color: var(--danger); }
.risk-text { margin: 0; font-size: 12.5px; line-height: 1.6; color: var(--text-2); }
.risk-actions { display: flex; justify-content: flex-end; gap: 8px; }
.risk-actions .btn { min-height: 36px; padding: 0 14px; border-radius: 8px; font-size: 13px; }
.risk-actions .btn.ghost { background: var(--fill); color: var(--text-2); }
.risk-actions .btn.danger { background: var(--danger); color: #fff; }
.risk-actions .btn.primary { background: var(--blue); color: #fff; }
.cmd-title { margin: 0; font-size: 15px; font-weight: 750; color: var(--text); }
.mode-notice {
  position:fixed;left:20px;right:20px;bottom:clamp(150px,28dvh,280px);z-index:95;
  max-width:360px;margin:0 auto;padding:15px 16px;border:1px solid var(--blue-border);
  border-radius:16px;background:var(--bg);color:var(--text);box-shadow:var(--shadow-2);
  animation:control-pop .2s ease both;
}
.mode-notice-head{display:flex;align-items:center;gap:8px;color:var(--blue);font-size:13px}
.mode-notice-head button{display:grid;place-items:center;margin-left:auto;width:28px;height:28px;border:0;background:none;color:var(--text-3)}
.mode-notice p{margin:8px 0 12px;color:var(--text-2);font-size:12px;line-height:1.6}
.mode-config{display:flex;align-items:center;justify-content:space-between;width:100%;min-height:36px;padding:0 10px;border:0;border-radius:9px;background:var(--blue-soft);color:var(--blue);font-size:12px}
.control-pill.on { background: var(--danger-soft); color: var(--danger); }
.control-bar { display: flex; align-items: center; gap: 8px; }
.control-bar .model-pick { margin-right: auto; }
.ctrl-perms { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; }
.perm-chip {
  height: 30px; padding: 0 11px; border-radius: var(--r-pill);
  border: 1px solid var(--orange-border, var(--border));
  background: none; color: var(--orange, var(--text-2));
  font-size: 12.5px; font-weight: 600;
}
.perm-chip.ok { border-color: var(--border); color: var(--ok); background: var(--fill); }
.perm-note { font-size: 11.5px; color: var(--text-3); }
.control-label { font-size: 12.5px; color: var(--text-2); font-weight: 650; }
.exit-control { height: 30px; padding: 0 11px; border-radius: var(--r-pill); background: var(--fill); color: var(--text-2); font-size: 12.5px; }
.control-input {
  width: 100%; min-height: 46px; padding: 10px 12px; border: 1px solid var(--border);
  border-radius: 16px; background: var(--fill); color: var(--text);
  font: inherit; font-size: 15.5px; outline: none; resize: none;
}
.control-thinking { font-size: 12px; color: var(--blue); line-height: 1.5; }
.control-chat-btn {
  align-self: flex-start; height: 34px; padding: 0 16px; border-radius: var(--r-pill);
  background: var(--blue); color: #fff; font-size: 13px; font-weight: 700;
}
.edit-banner {
  display: flex; align-items: center; justify-content: space-between; gap: 8px;
  margin: 0 2px 6px; padding: 6px 12px;
  border: 1px solid color-mix(in srgb, var(--blue) 40%, var(--border));
  border-radius: var(--r-pill);
  background: var(--blue-soft); color: var(--blue);
  font-size: 12px;
}
.edit-banner button { border: 0; background: none; color: var(--blue); font-weight: 650; }
.transfer { display: flex; align-items: center; gap: 8px; margin: 0 2px 6px; font-size: 11.5px; color: var(--text-2); }
.transfer span { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.transfer progress { width: 76px; height: 4px; accent-color: var(--blue); }

.field {
  position: relative;
  padding: 4px 6px 6px 8px;
  border: 1px solid var(--border);
  border-radius: 26px;
  background: var(--fill);
  transition: border-color .16s;
}
.life-orbit {
  position: absolute; z-index: 2; top: -13px; left: 50%;
  width: 27px; height: 27px; margin-left: -13.5px;
  border-radius: 50%; background: var(--bg);
  box-shadow: 0 0 0 3px var(--bg), 0 0 13px color-mix(in srgb, var(--blue) 38%, transparent);
  border: 0; padding: 0; cursor: pointer;
}
.orbit { position: absolute; inset: 3px; border-radius: 50%; }
.orbit.outer {
  border: 2px solid transparent; border-top-color: var(--blue); border-right-color: var(--blue);
  filter: drop-shadow(0 0 3px color-mix(in srgb, var(--blue) 70%, transparent));
  animation: life-spin 1.8s linear infinite;
}
.orbit.inner {
  inset: 7px; border: 2px solid transparent; border-bottom-color: var(--orange); border-left-color: var(--orange);
  filter: drop-shadow(0 0 2px color-mix(in srgb, var(--orange) 65%, transparent));
  animation: life-spin-reverse 1.15s linear infinite;
}
@keyframes life-spin { to { transform: rotate(360deg); } }
@keyframes life-spin-reverse { to { transform: rotate(-360deg); } }
@media (prefers-reduced-motion: reduce) {
  .orbit.outer, .orbit.inner { animation-duration: 6s; }
}
.field:focus-within { border-color: var(--blue-border); background: var(--bg); }
.field.busy { border-color: var(--border-strong); }

.composer-content { max-height: 132px; overflow-y: auto; overflow-x: hidden; border-radius: 18px 18px 8px 8px; padding-top: 3px; scrollbar-width: thin; }
.composer-content :deep(.attachment-strip) { padding: 4px 6px; }
.attachments { display:flex; flex-wrap:wrap; gap:6px; padding:7px 6px 2px; }
.attachment { display:inline-flex; align-items:center; gap:5px; max-width:100%; height:30px; padding:0 7px 0 9px; border:1px solid var(--blue-border); border-radius:10px; background:var(--blue-soft); color:var(--blue); font-size:12px; }
.attachment > span { min-width:0; max-width:190px; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }
.attachment button { display:grid; place-items:center; width:20px; height:20px; padding:0; border:0; border-radius:50%; background:transparent; color:inherit; }
.attachment button:active { background:color-mix(in srgb,var(--blue) 12%,transparent); }
.input {
  display: block; width: 100%; overflow-y: hidden;
  padding: 9px 10px 5px 6px; border: 0; background: none; outline: none; resize: none;
  font: inherit; font-size: 15.5px; line-height: 1.5; color: var(--text);
  scrollbar-width: thin; scrollbar-color: var(--border-strong) transparent;
}
.input.scrollable { overflow-y: auto; }
.input::placeholder { color: var(--text-3); }
.input:not(.scrollable)::-webkit-scrollbar { display: none; width: 0; }
.input.scrollable::-webkit-scrollbar { width: 3px; }
.input.scrollable::-webkit-scrollbar-track { margin-block: 12px 7px; background: transparent; }
.input.scrollable::-webkit-scrollbar-thumb { border-radius: 3px; background: var(--border-strong); }

.bar {
  display: flex; align-items: center; gap: 6px; padding: 2px 0 0 2px;
  /* 小屏/大字体机型：一行放不下就换行，而不是横向溢出屏幕。 */
  flex-wrap: wrap; min-width: 0;
}
.spacer { flex: 1 1 auto; min-width: 4px; }
/* 模式按钮浮在输入框上方，保留展开动画。 */
.mode-pop {
  position: absolute; z-index: 30; left: 10px; right: 10px; bottom: calc(100% + 10px);
  display: flex; flex-direction: row; align-items: center; justify-content: flex-start;
  flex-wrap: wrap; gap: 6px;
  /* 紧凑底部卡片随内容换行。 */
  background: var(--bg-card); border: 1px solid var(--border); border-radius: 16px;
  box-shadow: 0 4px 18px rgba(0, 0, 0, .08); padding: 8px;
}
.mode-pop .pill {
  /* 保持与以前一样大小的按钮 */
  min-height: 0; width: auto;
}
.mode-pop-enter-active, .mode-pop-leave-active { transition: opacity .18s ease, transform .22s cubic-bezier(.22,.9,.28,1.1); }
.mode-pop-enter-from, .mode-pop-leave-to { opacity: 0; transform: translateY(4px) scale(.97); }
.bar-toggle { flex-shrink: 0; }
.bar-toggle.on { color: var(--blue); background: var(--blue-soft); }
.production-pill.on { color: #ff3b30; background: rgba(255, 59, 48, 0.12); }

.act {
  /* 用自然尺寸而不是固定像素：系统开启大字模式时，图标与按钮随之缩放，不会错位。 */
  display: grid; place-items: center;
  min-width: 34px; min-height: 34px;
  padding: 6px;
  border: 0; border-radius: 50%; background: none; color: var(--text-2);
}
.act:active { background: var(--fill-press); }

.send {
  position: relative;
  display: grid; place-items: center; flex-shrink: 0;
  /* aspect-ratio 1 + min 尺寸：正常字号下仍是正圆，大字体下整体放大不破形。 */
  min-width: 36px; min-height: 36px; aspect-ratio: 1; padding: 6px;
  border: 0; border-radius: 50%;
  background: var(--blue); color: #fff;
  transition: background .16s, transform .06s;
}
.send.jump { background: var(--orange); }
.send.stop { background: var(--text); }
.send:disabled { background: var(--border-strong); pointer-events: none; }
.send:active { transform: scale(.92); }
.send-ripple { position: absolute; inset: 0; border-radius: inherit; pointer-events: none; animation: send-ripple-life .5s linear; }
.send-ripple i { position: absolute; inset: 0; border: 1px solid color-mix(in srgb, var(--blue) 35%, transparent); border-radius: inherit; background: transparent; opacity: 0; animation: send-ripple-expand .42s ease-out forwards; }
.send-ripple i + i { animation-delay: .08s; }
@keyframes send-ripple-life { from { opacity: 1; } to { opacity: 1; } }
@keyframes send-ripple-expand { 0% { opacity: .65; transform: scale(.8); } 100% { opacity: 0; transform: scale(2.6); } }
@media (prefers-reduced-motion: reduce) { .send-ripple { display: none; } }

/* 指令面板浮层：可滚动卡片 */
.quick-scrim { position: fixed; inset: 0; z-index: 1; }
.quick {
  position: absolute; z-index: 2; left: 10px; right: 10px; bottom: calc(100% + 4px);
  max-height: min(56vh, 360px); overflow-y: auto;
  padding: 10px 12px 12px;
  border: 1px solid var(--border); border-radius: var(--r-card);
  background: var(--bg); box-shadow: var(--shadow-2);
  animation: coomi-cascade .18s ease both;
}
.reasoning-head { margin-top: 3px; }
.reasoning-options { display:grid; grid-template-columns:repeat(5,minmax(0,1fr)); gap:4px; margin-bottom:8px; }
.reasoning-options button { position:relative; min-width:0; height:32px; overflow:hidden; isolation:isolate; border-radius:7px; background:var(--fill); color:var(--text-2); font-size:12px; transition:background 160ms ease,color 160ms ease,box-shadow 160ms ease,transform 120ms ease; }
.reasoning-options button.selected { background:color-mix(in srgb,var(--blue-soft) 84%,var(--bg)); color:var(--blue); font-weight:650; box-shadow:inset 0 0 0 1px color-mix(in srgb,var(--blue) 13%,transparent); }
.reasoning-options button:active { transform:scale(.96); }
.effort-label { position:relative; z-index:2; }
.effort-particles { position:absolute; z-index:1; inset:0; overflow:hidden; border-radius:inherit; pointer-events:none; }
.effort-particle {
  position:absolute; left:var(--px); top:var(--py); width:var(--particle-size); height:var(--particle-size);
  border-radius:50%; background:color-mix(in srgb,var(--blue) 82%,white);
  box-shadow:0 0 var(--particle-glow) color-mix(in srgb,var(--blue) 72%,transparent);
  opacity:0; animation:effort-particle-rise var(--particle-speed) ease-in-out infinite;
}
.reasoning-options button[data-effort='auto'] { --particle-size:2px; --particle-glow:4px; --particle-alpha:.42; --particle-rise:-7px; --particle-speed:2.8s; }
.reasoning-options button[data-effort='low'] { --particle-size:1.5px; --particle-glow:3px; --particle-alpha:.28; --particle-rise:-5px; --particle-speed:3.2s; }
.reasoning-options button[data-effort='medium'] { --particle-size:2px; --particle-glow:4px; --particle-alpha:.48; --particle-rise:-7px; --particle-speed:2.7s; }
.reasoning-options button[data-effort='high'] { --particle-size:2.5px; --particle-glow:5px; --particle-alpha:.68; --particle-rise:-9px; --particle-speed:2.25s; }
.reasoning-options button[data-effort='xhigh'] { --particle-size:3px; --particle-glow:7px; --particle-alpha:.88; --particle-rise:-11px; --particle-speed:1.9s; }
.effort-particle:nth-child(1) { --px:13%; --py:72%; --drift-x:-2px; animation-delay:-.2s; }
.effort-particle:nth-child(2) { --px:76%; --py:67%; --drift-x:2px; animation-delay:-1.05s; }
.effort-particle:nth-child(3) { --px:46%; --py:78%; --drift-x:-1px; animation-delay:-1.7s; }
.effort-particle:nth-child(4) { --px:88%; --py:43%; --drift-x:-2px; animation-delay:-.65s; }
.effort-particle:nth-child(5) { --px:27%; --py:48%; --drift-x:2px; animation-delay:-1.35s; }
.effort-particle:nth-child(6) { --px:61%; --py:53%; --drift-x:1px; animation-delay:-.4s; }
.effort-particle:nth-child(7) { --px:7%; --py:39%; --drift-x:2px; animation-delay:-1.55s; }
.effort-particle:nth-child(8) { --px:94%; --py:78%; --drift-x:-1px; animation-delay:-.9s; }
.reasoning-options button:nth-child(2).selected::after { inset:-5px; }
.reasoning-options button:nth-child(3).selected::after { inset:-8px; }
.reasoning-options button:nth-child(4).selected::after, .reasoning-options button:nth-child(5).selected::after { inset:-11px; }
.life-stats-card { position:absolute; z-index:4; left:12px; right:12px; bottom:calc(100% + 10px); overflow:hidden; padding:11px 12px 12px; border:1px solid color-mix(in srgb,var(--blue) 35%,var(--border)); border-radius:13px; background:color-mix(in srgb,var(--bg) 94%,var(--blue-soft)); box-shadow:0 8px 28px color-mix(in srgb,var(--blue) 20%,transparent); animation:life-card-in .2s ease both; }
.life-stats-card header { display:flex; align-items:center; justify-content:space-between; color:var(--text); font-size:12px; font-weight:650; }
.life-stats-card header button { display:grid; place-items:center; width:24px; height:24px; border-radius:50%; background:var(--fill); color:var(--text-2); }
.life-waveform { position:relative; height:64px; margin:9px 1px 10px; overflow:hidden; border-radius:7px; background:linear-gradient(to bottom, color-mix(in srgb,var(--blue-soft) 28%,transparent), transparent 50%, color-mix(in srgb,var(--blue-soft) 20%,transparent)); }
.life-waveform svg { display:block; width:100%; height:100%; overflow:visible; }
.wave { transform-origin:160px 50px; vector-effect:non-scaling-stroke; animation:life-wave-breathe 3.4s ease-in-out infinite alternate; }
.wave-upper { fill:color-mix(in srgb,var(--orange) 48%,var(--bg)); stroke:color-mix(in srgb,var(--orange) 72%,var(--bg)); stroke-width:1.2; }
.wave-lower { fill:color-mix(in srgb,var(--blue) 52%,var(--bg)); stroke:color-mix(in srgb,var(--blue) 76%,var(--bg)); stroke-width:1.2; }
.upper-back,.lower-back { opacity:.55; animation-duration:4.5s; animation-delay:-1.1s; }
.upper-front,.lower-front { opacity:.7; animation-duration:2.8s; animation-delay:-.55s; }
.wave-baseline { fill:none; stroke:var(--border-strong); stroke-width:1.4; vector-effect:non-scaling-stroke; opacity:.9; }
.life-stats-grid { display:grid; grid-template-columns:repeat(2,1fr); gap:7px 12px; }.life-stats-grid span { display:flex; justify-content:space-between; gap:8px; color:var(--text-3); font-size:11px; }.life-stats-grid strong { color:var(--text-2); font-weight:600; }
@keyframes life-card-in { from { opacity:0; transform:translateY(5px) scale(.98); } to { opacity:1; transform:none; } }
@keyframes reasoning-ripple { 0%,100% { transform:scale(.92); opacity:.35; } 50% { transform:scale(1.12); opacity:.9; } }
@keyframes life-wave-breathe { 0% { transform:translateX(-1.5px) scaleY(.9); opacity:.72; } 50% { transform:translateX(0) scaleY(1.04); opacity:1; } 100% { transform:translateX(1.5px) scaleY(.94); opacity:.8; } }
@media (prefers-reduced-motion: reduce) { .wave { animation-duration:8s; } .effort-particle { animation:none; opacity:.24; } }
  .local-actions { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 7px; margin-bottom: 8px; }
  .local-actions .qchip { justify-content: flex-start; }
.qhead { margin-bottom: 8px; font-size: 12px; font-weight: 600; color: var(--text-3); }
.file-actions { display: flex; gap: 7px; margin-bottom: 8px; }
.qchip.file { display: inline-flex; align-items: center; gap: 5px; color: var(--blue); }
.qchip {
  height: 32px; padding: 0 13px;
  border: 1px solid var(--border); border-radius: var(--r-pill);
  background: var(--bg); font-size: 13.5px; color: var(--text-2);
}
.qchip:active { background: var(--blue-soft); border-color: var(--blue-border); color: var(--blue); }

/* 斜杠指令逐行列表 */
.slash-list { display: flex; flex-direction: column; gap: 2px; }
.slash-item {
  display: flex; align-items: center; gap: 10px; width: 100%;
  padding: 10px 8px; border: 0; border-radius: 10px;
  background: none; text-align: left; cursor: pointer;
}
.slash-item code { font-family: inherit; font-size: 13.5px; font-weight: 700; color: var(--blue); }
.slash-item span { font-size: 12.5px; color: var(--text-2); }
.slash-item:active { background: var(--blue-soft); }
</style>
