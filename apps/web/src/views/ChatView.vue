<script setup lang="ts">
/**
 * 主聊天窗口。
 *
 * 三件事和别处不一样：
 * 1) 抽屉打开时整个 shell 往右推 + 轻微缩放，是 DeepSeek 的那种层次感；
 * 2) 跟随滚动交给 useAutoScroll，高度变化用 ResizeObserver 兜住 ——
 *    markdown 重排、工具卡展开、软键盘弹出都会改高度，只 watch 数组长度会漏；
 * 3) 连续的工具调用合并成一个 ToolGroup，避免长任务把时间线冲成一堵卡片墙。
 */
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { DynamicScroller, DynamicScrollerItem } from 'vue-virtual-scroller'
import 'vue-virtual-scroller/dist/vue-virtual-scroller.css'
import { useRouter } from 'vue-router'
import { useSessionStore } from '@/stores/session'
import { useSessionsStore } from '@/stores/sessions'
import { useConfigStore } from '@/stores/config'
import { apiGet } from '@/bridge/http'
import { DEMO_PROMPT, isUnattended, shouldAutoplay } from '@/bridge/demoMode'
import { useAutoScroll } from '@/composables/useAutoScroll'
import type { ToolCard } from '@/stores/viewModel'
import { buildTimelineBlocks, type TimelineBlockItem } from '@/utils/chatTimeline'
import type { ApprovalDecision } from '@/protocol/commands'
import TopBar from '@/components/TopBar.vue'
import SideDrawer from '@/components/SideDrawer.vue'
import ArtifactMindMap from '@/components/ArtifactMindMap.vue'
import StatusBar from '@/components/StatusBar.vue'
import Composer from '@/components/Composer.vue'
import EmptyState from '@/components/EmptyState.vue'
import TimelineBlock from '@/components/TimelineBlock.vue'
import LoopProgressBar from '@/components/LoopProgressBar.vue'
import ApprovalSheet from '@/components/ApprovalSheet.vue'
import QuestionSheet from '@/components/QuestionSheet.vue'
import CoomiIcon from '@/components/CoomiIcon.vue'
import { registerOverlay, unregisterOverlay } from '@/bridge/overlayStack'
import { chatScroll } from '@/stores/viewState'

const composerModesOpen = ref(false)
const router = useRouter()
const session = useSessionStore()
const sessions = useSessionsStore()
const config = useConfigStore()

const scroller = ref<HTMLElement | null>(null)
const virtualScroller = ref<InstanceType<typeof DynamicScroller> | null>(null)
const content = ref<HTMLElement | null>(null)
const drawerOpen = ref(false)
/** 产物思维导图抽屉：从右边缘左滑打开，与左侧会话抽屉互斥。 */
const mindMapOpen = ref(false)
let mindMapTouchStartX = 0
let mindMapTouchStartY = 0
let mindMapTouching = false
/** 全局轮询「后台运行中」状态的定时器（会话列表转圈的数据源）。 */
let runningPoll: ReturnType<typeof setInterval> | null = null

const { following, follow, jumpToBottom } = useAutoScroll(scroller)
const savedScroll = chatScroll.get(session.sessionId)
if (savedScroll) following.value = savedScroll.following

const positioning = ref(false)
let positionRevision = 0
watch([() => session.sessionId, () => session.viewRevision, () => session.restoring], async () => {
  const revision = ++positionRevision
  positioning.value = true
  if (session.restoring) return
  await nextTick()
  await new Promise<void>(resolve => requestAnimationFrame(() => resolve()))
  if (revision !== positionRevision) return
  jumpToBottom()
  await new Promise<void>(resolve => requestAnimationFrame(() => resolve()))
  if (revision === positionRevision) positioning.value = false
}, { flush: 'sync' })

const blocks = computed<TimelineBlockItem[]>(() => buildTimelineBlocks(session.timeline, session.isBusy))

function syncDigitalLifeMode() {
  config.syncDigitalLifeEnabled()
  if (!session.isBusy) session.syncLifeMode()
}

let ro: ResizeObserver | null = null

onMounted(() => {
  session.connect()
  window.addEventListener('coomi:flush-persistence', session.flushPersistence)
  // 记录引擎当前工作目录，会话列表据此把不同项目的会话隔离开。
  void apiGet<{ cwd?: string }>('/api/runtime/health')
    .then(h => { if (h?.cwd) sessions.setCurrentCwd(h.cwd) })
    .catch(() => { /* 引擎未就绪时保持空 cwd，列表退化为全部显示 */ })
  // 以引擎磁盘会话为权威源同步列表，修复“会话记录消失/串会话”。
  void sessions.syncFromEngine()
  if (config.providers.length === 0) void config.fetchProviders()
  // 全局记忆开关以引擎为权威：启动即同步，避免「开关显示关、引擎实际开」的脱节。
  void config.syncGlobalMemoryFromEngine()
  syncDigitalLifeMode()
  window.addEventListener('focus', syncDigitalLifeMode)
  document.addEventListener('visibilitychange', session.resumeFromBackground)
  // 全局轮询各会话的「后台运行中」状态：切走会话后任务在引擎侧继续跑，
  // 抽屉/会话页据此显示转圈。轮询常驻（本地 API 开销极小），不依赖抽屉打开。
  void sessions.refreshRunning()
  runningPoll = setInterval(() => { sessions.refreshRunning(); void session.refreshLifeUnread(); session.autoDeliverLifeIfReady() }, 2000)
  void session.refreshLifeUnread()
  // 高度只要变就重新贴底（内部有 rAF 合并，不怕高频触发）
  if (typeof ResizeObserver !== 'undefined') {
    ro = new ResizeObserver(() => follow())
    if (content.value) ro.observe(content.value)
    if (scroller.value) ro.observe(scroller.value)
  }
  nextTick(() => {
    if (savedScroll && !savedScroll.following && scroller.value) scroller.value.scrollTop = savedScroll.top
    else follow()
  })
  // 演示模式自动播一轮，省得进来还要先打字才能看见瀑布流。
  if (shouldAutoplay() && session.timeline.length === 0) {
    setTimeout(() => { if (session.timeline.length === 0) session.sendMessage(DEMO_PROMPT) }, 700)
  }
  window.addEventListener('touchstart', onMindMapTouchStart, { passive: true })
  window.addEventListener('touchmove', onMindMapTouchMove, { passive: true })
  window.addEventListener('touchend', onMindMapTouchEnd)
})

onBeforeUnmount(() => {
  if (scroller.value) chatScroll.set(session.sessionId, { top: scroller.value.scrollTop, following: following.value })
  unregisterOverlay('side-drawer')
  unregisterOverlay('artifact-mindmap')
  window.removeEventListener('coomi:flush-persistence', session.flushPersistence)
  window.removeEventListener('focus', syncDigitalLifeMode)
  document.removeEventListener('visibilitychange', session.resumeFromBackground)
  positionRevision += 1
  session.flushPersistence()
  window.removeEventListener('touchstart', onMindMapTouchStart)
  window.removeEventListener('touchmove', onMindMapTouchMove)
  window.removeEventListener('touchend', onMindMapTouchEnd)
  if (runningPoll) { clearInterval(runningPoll); runningPoll = null }
  ro?.disconnect(); ro = null
})

/**
 * 无人值守演示（?demo=1&auto=1）：授权弹层和提问弹层过一会儿自己点掉。
 * 走的是 approve / answerQuestion —— 和真手指按下去完全同一条路，
 * 所以卡片状态、「已回答」气泡都跟着变。截图、录屏、摆着自演都靠它。
 */
if (isUnattended()) {
  const AUTOPILOT_DELAY = 1600
  watch(() => session.pendingApproval?.callId, id => {
    if (!id) return
    setTimeout(() => { if (session.pendingApproval?.callId === id) session.approve(id, 'allow') }, AUTOPILOT_DELAY)
  })
  watch(() => session.pendingQuestion?.callId, id => {
    if (!id) return
    setTimeout(() => {
      const q = session.pendingQuestion
      if (q?.callId === id) {
        session.answerQuestion(id, Object.fromEntries(q.questions.map(question => [question.id, question.options[0]?.label ?? ''])))
      }
    }, AUTOPILOT_DELAY)
  })
}

// ResizeObserver 不可用时的兜底：至少条目增减能跟上。
watch(() => session.timeline.length, () => nextTick(follow))
// 流式输出时气泡高度持续变化，但 DynamicScroller 内部容器不是 ResizeObserver 观察对象：
// usage_update（输出 token 增长时持续推送）是可靠的「内容在变」信号，据此贴底。
watch(() => session.usage?.output, () => follow(), { deep: false })
watch(() => session.usage?.turnOutputTokens, () => follow(), { deep: false })
watch([() => config.digitalLifeEnabled, () => config.lifeGlobalMode, () => session.isBusy], ([, , busy]) => {
  if (!busy) session.syncLifeMode()
})

function onDecide(callId: string, decision: ApprovalDecision) { session.approve(callId, decision) }
function onAnswer(callId: string, answers: Record<string, string>) { session.answerQuestion(callId, answers) }
function openDrawer() { drawerOpen.value = true; registerOverlay('side-drawer', closeDrawer) }
function closeDrawer() { drawerOpen.value = false; unregisterOverlay('side-drawer') }
function openMindMap() { mindMapOpen.value = true; registerOverlay('artifact-mindmap', closeMindMap) }
function closeMindMap() { mindMapOpen.value = false; unregisterOverlay('artifact-mindmap') }

/** 从右边缘左滑打开产物思维导图 */
function onMindMapTouchStart(e: TouchEvent) {
  if (e.touches.length !== 1 || mindMapOpen.value) return
  mindMapTouchStartX = e.touches[0].clientX
  mindMapTouchStartY = e.touches[0].clientY
  mindMapTouching = true
}
function onMindMapTouchMove(e: TouchEvent) {
  if (!mindMapTouching) return
  const dx = e.touches[0].clientX - mindMapTouchStartX
  const dy = e.touches[0].clientY - mindMapTouchStartY
  if (Math.abs(dx) < Math.abs(dy)) return
  if (dx < -60 && mindMapTouchStartX > window.innerWidth - 40) {
    openMindMap()
    mindMapTouching = false
  }
}
function onMindMapTouchEnd() { mindMapTouching = false }

watch(() => session.pendingApproval?.callId, (id, previous) => {
  if (previous) unregisterOverlay(`approval:${previous}`)
  if (id) registerOverlay(`approval:${id}`, () => session.approve(id, 'deny'))
})
watch(() => session.pendingQuestion?.callId, (id, previous) => {
  if (previous) unregisterOverlay(`question:${previous}`)
  if (id) registerOverlay(`question:${id}`, () => session.answerQuestion(id, {}))
})
</script>

<template>
  <div class="chat" :class="{ 'minimal-ui': config.minimalUi }">
    <div class="shell" :class="{ pushed: drawerOpen }">
      <TopBar :menu-open="drawerOpen" @menu="openDrawer" />

      <main ref="scroller" class="stream" :style="{ visibility: session.restoring || positioning ? 'hidden' : 'visible' }" :aria-busy="session.restoring">
        <div v-if="session.timeline.length === 0" ref="content" class="inner empty-inner">
          <EmptyState v-if="session.timeline.length === 0" />
        </div>
        <DynamicScroller
          v-else
          :key="`${session.sessionId}:${session.viewRevision}`"
          ref="virtualScroller"
          :items="blocks"
          key-field="key"
          :min-item-size="48"
          :buffer="640"
          class="virtual-stream"
          page-mode
        >
          <template #default="{ item, index, active }">
            <DynamicScrollerItem
              :item="item"
              :active="active"
              :size-dependencies="item.t === 'one' ? [item.item] : item.t === 'tools' ? [item.cards] : [item.blocks, item.running]"
              :data-index="index"
              class="virtual-item"
            >
              <TimelineBlock :block="item" />
            </DynamicScrollerItem>
          </template>
        </DynamicScroller>
      </main>

      <Transition name="pop">
        <button v-if="!following && !composerModesOpen" class="to-bottom" aria-label="回到底部" @click="jumpToBottom">
          <CoomiIcon name="arrowDown" :size="18" />
        </button>
      </Transition>

      <LoopProgressBar v-if="(session.plan.active && session.plan.steps.length) || session.loop.active" />
      <div v-if="session.retryConfirmation" class="retry-confirm">
        <div><CoomiIcon name="alert" :size="16" /><span>{{ session.retryConfirmation }}</span></div>
        <div class="retry-actions">
          <button class="retry-secondary" @click="session.dismissRetry()">结束任务</button>
          <button class="retry-primary" @click="session.retryInterruptedTurn()">继续重试</button>
        </div>
      </div>
      <div v-if="session.undoConfirm" class="retry-confirm">
        <div><CoomiIcon name="arrowLeft" :size="16" /><span>回撤这一轮</span></div>
        <p v-if="session.undoConfirm.busy">正在核对或恢复，请稍候…</p>
        <p v-if="session.undoConfirm.error" role="alert">{{ session.undoConfirm.error }}</p>
        <template v-if="session.undoConfirm.preview">
          <p>仅撤回对话会保留已执行的修改。恢复文件只处理下列本轮记录的修改，Git 提交和暂存区保持不变。</p>
          <ul v-if="session.undoConfirm.preview.files.length" class="undo-files">
            <li v-for="file in session.undoConfirm.preview.files" :key="file.path">{{ file.action === 'delete' ? '移除新增文件' : '恢复原内容' }}：{{ file.path }}</li>
          </ul>
          <p v-else>没有可恢复的文件修改。</p>
          <p v-for="warning in session.undoConfirm.preview.warnings" :key="warning">{{ warning }}</p>
          <p v-for="conflict in session.undoConfirm.preview.conflicts" :key="conflict" role="alert">{{ conflict }}</p>
          <p v-if="session.undoConfirm.preview.conflicts.length">文件恢复已阻止；后续修改会保留。仍可选择仅撤回对话。</p>
        </template>
        <div class="retry-actions">
          <button class="retry-secondary" :disabled="session.undoConfirm.busy" @click="session.cancelUndo()">取消</button>
          <button v-if="session.undoConfirm.error || session.undoConfirm.preview?.conflicts.length" class="retry-secondary" :disabled="session.undoConfirm.busy" @click="session.requestUndo(session.undoConfirm.mid)">重新预览</button>
          <button class="retry-secondary" :disabled="session.undoConfirm.busy || !session.undoConfirm.preview" @click="session.confirmUndo(false)">仅撤回对话</button>
          <button v-if="session.undoConfirm.preview?.files.length" class="retry-primary" :disabled="session.undoConfirm.busy || !!session.undoConfirm.preview?.conflicts.length" @click="session.confirmUndo(true)">恢复文件并撤回</button>
        </div>
      </div>
      <Transition name="pop">
        <button
          v-if="session.lifeUnread.length && session.isGlobalSession && session.mode === 'life' && !session.isBusy"
          class="life-pill"
          type="button"
          @click="session.deliverLife()"
        >
          <CoomiIcon name="lifeRings" :size="15" />
          <span class="life-pill-label">{{ session.lifeUnreadName || '数字生命体' }} 想对你说</span>
          <span class="life-pill-preview">{{ session.lifeUnread[0].text }}</span>
        </button>
      </Transition>
      <StatusBar v-if="!composerModesOpen" />
      <Composer @modes-open="composerModesOpen = $event" />
    </div>

    <SideDrawer :open="drawerOpen" @close="closeDrawer" />
    <ArtifactMindMap :open="mindMapOpen" @close="closeMindMap" />

    <ApprovalSheet
      v-if="session.pendingApproval"
      :card="session.pendingApproval"
      @decide="(d: ApprovalDecision) => onDecide(session.pendingApproval!.callId, d)"
    />
    <QuestionSheet
      v-else-if="session.pendingQuestion"
      :card="session.pendingQuestion"
      @answer="(answers: Record<string, string>) => onAnswer(session.pendingQuestion!.callId, answers)"
    />
  </div>
</template>

<style scoped>
.chat {
  height: 100%;
  min-height: 0;
  background-color: var(--bg);
  background-image:
    linear-gradient(var(--chat-background-overlay), var(--chat-background-overlay)),
    var(--chat-background-image);
  background-position: center;
  background-size: cover;
  overflow-x: clip;
}


.shell {
  position: relative;
  display: flex; flex-direction: column; height: 100%; min-height: 0;
  background: var(--bg);
  transform-origin: right center;
  /* 只保留 transform 动画：Android WebView 里 transform+border-radius 同时
     过渡会反复重建合成层，表现为打开侧边栏时主内容文字闪烁。
     will-change 让合成层常驻，避免动画开始/结束时闪一下。 */
  transition: transform .3s cubic-bezier(.22, .68, .19, 1);
  will-change: transform;
}
.shell.pushed {
  /* 右侧原点固定边缘，只缩放一次，不再叠加百分比平移露出 WebView 背景。 */
  transform: scale(.94);
  border-radius: 20px;
  overflow: hidden;
}

.stream {
  flex: 1; min-width: 0; min-height: 0; max-width: 100%; overflow-x: hidden; overflow-y: auto;
  -webkit-overflow-scrolling: touch; overscroll-behavior-y: contain;
}
.inner {
  display: flex; flex-direction: column; gap: 12px;
  width: 100%; min-width: 0; min-height: 100%; padding: 10px 12px 18px; overflow-x: hidden;
}
.empty-inner { min-height: 100%; }
.virtual-stream { width: 100%; min-width: 0; padding: 10px 12px 18px; overflow: visible; }
.virtual-item { width: 100%; min-width: 0; padding-bottom: 12px; }

.to-bottom {
  position: absolute; left: 50%; bottom: 116px; z-index: 8;
  display: grid; place-items: center;
  width: 38px; height: 38px; margin-left: -19px;
  border: 1px solid var(--border); border-radius: 50%;
  background: var(--bg); color: var(--text-2);
  box-shadow: var(--shadow-2);
}
.to-bottom:active { background: var(--fill); }
.pop-enter-active, .pop-leave-active { transition: opacity .18s ease, transform .18s ease; }
.pop-enter-from, .pop-leave-to { opacity: 0; transform: translateY(8px) scale(.9); }

.life-pill {
  display: flex; align-items: center; gap: 7px;
  margin: 0 12px 6px; padding: 8px 12px;
  border: 1px solid color-mix(in srgb, var(--accent) 45%, var(--border));
  border-radius: 12px;
  background: linear-gradient(105deg, color-mix(in srgb, var(--accent-soft) 80%, var(--bg)), var(--bg));
  color: var(--accent);
  box-shadow: var(--shadow-1);
  text-align: left;
}
.life-pill-label { flex: none; font-size: 12.5px; font-weight: 650; }
.life-pill-preview {
  flex: 1; min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis;
  color: var(--text-3); font-size: 12px;
}

.retry-confirm { margin: 0 12px 6px; padding: 10px 12px; border: 1px solid var(--border); border-radius: 8px; background: var(--bg); box-shadow: var(--shadow-1); max-height: 45vh; overflow-y: auto; }.retry-confirm > div:first-child { display: flex; align-items: center; gap: 7px; font-size: 13px; color: var(--text-2); }
.undo-files { max-height: 160px; overflow-y: auto; padding-left: 18px; font-size: 12px; overflow-wrap: anywhere; }
.retry-actions { display: flex; justify-content: flex-end; gap: 8px; margin-top: 9px; }
.retry-actions button { min-height: 34px; padding: 0 13px; border-radius: 6px; font-size: 13px; font-weight: 600; }
.retry-secondary { background: var(--fill); color: var(--text-2); }
.retry-primary { background: var(--blue); color: #fff; }
</style>
