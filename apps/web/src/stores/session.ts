import { defineStore } from 'pinia'
import { RecoveryEvents } from '@/utils/sessionRecovery'
import { buildAttachmentRequest, parseAttachmentRequest, type ChatAttachment } from '@/utils/attachments'
import { ref, computed, shallowRef, watch, inject, hasInjectionContext, type Ref, type InjectionKey } from 'vue'
import { createTransport, type Transport } from '@/bridge'
import { authedFetch, apiGet, apiSend } from '@/bridge/http'
import { isDemoMode } from '@/bridge/demoMode'
import type { AgentEvent } from '@/protocol/events'
import type { ReasoningEffortStats } from '@/protocol/events'
import type { ReasoningEffort } from '@/protocol/commands'
import type { InboundEnvelope } from '@/protocol/commands'
import { nextId } from '@/bridge/envelope'
import { useConnectionStore, useScopedConnectionStore } from './connection'
import { useConfigStore } from './config'
import { useSessionsStore } from './sessions'
import { isGlobalSession as isGlobalSessionId } from '@/bridge/life'
import { router } from '@/router'
import { pushStatus as publishControlStatus, pushTrace as publishTrace, setControlModeActive } from '@/bridge/controlFloat'
import type { AssistantMessage, LoopProgress, PlanProgress, QuestionCard, ReasoningBlock, RunState, Timelineitem, ToolCard, ToolDiagnosticTrace, UserMessage } from './viewModel'

function defineSessionStore(storeId: string, auxiliary = false) {
  return defineStore(storeId, () => {
  const connection = auxiliary ? useScopedConnectionStore(storeId) : useConnectionStore()
  const config = useConfigStore()
  const sessions = useSessionsStore()

  const sessionId = ref(auxiliary ? createSessionId() : readActiveSessionId())
  const mode = ref<'agent' | 'life'>(sessions.find(sessionId.value)?.mode ?? 'agent')
  const timeline = ref<Timelineitem[]>(sessions.loadTranscript(sessionId.value))
  const runState = ref<RunState>('idle')
  const restoring = ref(false)
  const viewRevision = ref(0)
  let selectionEpoch = 0
  let restoreSerial = 0
  let restoreAbort: AbortController | null = null
  let pendingRestore: { id: string; epoch: number; promise: Promise<boolean> } | null = null
  let restoreEvents = new RecoveryEvents<AgentEvent>()
  const appliedThrough = new Map<string, number>()
  const handledFileRequests = new Set<string>()
  let replaying = false
  function pushControlStatus(text: string) { if (!replaying) publishControlStatus(text) }
  function pushTrace(title: string, text: string) { if (!replaying) publishTrace(title, text) }
  const usage = ref<{
    total: number; input: number; output: number; contextRatio: number
    contextUsed: number; contextWindow: number
    cachedInput: number; cacheHitRate: number | null; cacheDataAvailable: boolean
    turnCacheHitRate: number | null; turnCacheDataAvailable: boolean
    /** 本轮即时速率与耗时（后端新增字段，用于顶部/状态栏的实时速率显示）。 */
    turnRateTps: number
    turnElapsedMs: number
    turnOutputTokens: number
    reasoningEfforts: Partial<Record<ReasoningEffort, ReasoningEffortStats>>
    contextCategories: Partial<Record<'system_tools' | 'messages' | 'skills' | 'mcp_tools' | 'system_prompt' | 'other', number>>
  } | null>(null)
  const retryConfirmation = ref<string | null>(null)
  /** 当前会话的工作目录（会话标记路径，绑定为会话执行目录）。 */
  const cwd = ref('')
  const loop = ref<LoopProgress>({ active: false, currentStep: 0, totalSteps: 0, status: '' })
  /** 计划进度由引擎 plan_updated 事件驱动（完整步骤列表），与持久化 loop 分离。 */
  const plan = ref<PlanProgress>({ active: false, steps: [] })
  /** 生命体待投递问候（队列唯一 pending → 气泡/开场问候的数据源）。 */
  const lifeUnread = ref<LifeUnreadItem[]>([])
  const lifeUnreadName = ref('')
  const lifeDelivering = ref(false)
  const lifeStatsOpen = ref(false)
  /** 本次打开会话是否已做过开场问候（避免轮询重复触发投递）。 */
  let lifeAutoSent = false
  /** 编辑覆盖状态：非空表示输入框正处于「编辑上一条消息」模式。 */
  const pendingEdit = ref<{ mid: string; content: string } | null>(null)
  /** 回撤确认弹窗状态：非空表示等待用户确认回撤该轮。 */
  const undoConfirm = ref<{ mid: string; sessionId: string; busy: boolean; error: string; preview?: {
    turn_id: string; available: boolean; files: {path: string; action: string}[]; conflicts: string[]; warnings: string[]; revision: string
  } } | null>(null)

  let currentAssistant: AssistantMessage | null = null

  /**
   * 控制模式悬浮层的思考缓冲。
   *
   * reasoning_chunk 是按 token 来的，一秒能来几十条。逐条转发到原生层会不停跨进程调用、
   * 把悬浮层刷成走马灯。所以攒一段再送：要么攒够长度，要么静了 500ms。
   */
  let floatReasoningBuffer = ''
  let floatReasoningTimer: ReturnType<typeof setTimeout> | null = null

  function queueFloatReasoning(chunk: string) {
    if (replaying) return
    floatReasoningBuffer += chunk
    if (floatReasoningBuffer.length >= 300) { flushFloatReasoning(); return }
    if (floatReasoningTimer) return
    floatReasoningTimer = setTimeout(flushFloatReasoning, 500)
  }

  function flushFloatReasoning() {
    if (floatReasoningTimer) { clearTimeout(floatReasoningTimer); floatReasoningTimer = null }
    const text = floatReasoningBuffer.trim()
    floatReasoningBuffer = ''
    if (text) pushTrace('思考', text)
  }

  /**
   * 把工具参数压成一行给悬浮层看。
   *
   * 悬浮层只有一小条宽度，塞不下完整的参数 JSON；只取前几个键值，够让用户判断
   * 「它现在在动哪个文件 / 点哪个按钮」就行。
   */
  function describeFloatArguments(args: Record<string, unknown> | undefined): string {
    if (!args) return ''
    const parts: string[] = []
    for (const [key, value] of Object.entries(args)) {
      if (value == null || value === '') continue
      const text = typeof value === 'string' ? value : JSON.stringify(value)
      if (!text) continue
      parts.push(`${key}=${text}`)
      if (parts.length >= 4) break
    }
    return parts.join(' · ')
  }

  let connectedSessionId = ''
  /** 并行会话：每个已打开的会话各持一条 WS（后台任务不断连，切回即可看到结果）。 */
  const transportPool = new Map<string, Transport>()
  /** 池上限：防止长时间使用后连接无限增长（每个连接 + 引擎事件缓冲都占内存）。 */
  const TRANSPORT_POOL_MAX = 5
  let persistTimer: ReturnType<typeof setTimeout> | null = null
  let turnToolTrace: ToolDiagnosticTrace[] = []
  let consecutiveToolFailures = 0
  let maxConsecutiveToolFailures = 0
  let failureNoticeCreated = false
  const transport = shallowRef<Transport | null>(null)

  const isBusy = computed(() => restoring.value || runState.value !== 'idle')
  const pendingApproval = computed(() => timeline.value.find((t): t is ToolCard => t.kind === 'tool' && t.status === 'awaiting_approval'))
  const pendingQuestion = computed(() => timeline.value.find((t): t is QuestionCard => t.kind === 'question' && !t.answered))
  /** 时间线里最新的用户消息（仅它可「编辑重发」）。 */
  const lastUserMessage = computed(() => {
    for (let i = timeline.value.length - 1; i >= 0; i--) {
      if (timeline.value[i].kind === 'user') return timeline.value[i]
    }
    return null
  })
  /** 时间线里最新的助手消息（仅它可「回撤」）。 */
  const lastAssistantMessage = computed(() => {
    for (let i = timeline.value.length - 1; i >= 0; i--) {
      if (timeline.value[i].kind === 'assistant') return timeline.value[i]
    }
    return null
  })

  if (!auxiliary) persistActiveSessionId(sessionId.value)

  // Native task status is derived from /api/tasks in the sessions store. A
  // foreground idle session must not overwrite another session's running state.

  /** 发送内置引导（EmptyState 引导卡）：先置用户标题消息，再让引擎流式推正文。 */
  const GUIDE_TITLES: Record<string, string> = {
    newbie: 'Coomi 新手使用指南',
    extension: '自定义拓展进化指南',
  }
  function sendGuide(key: string) {
    const trimmed = (GUIDE_TITLES[key] ?? 'Coomi 指南').trim()
    // 首条用户消息作为会话标题，抽屉里就不会全是「新对话」。
    const isFirst = !timeline.value.some(t => t.kind === 'user')
    if (isFirst) sessions.touch(sessionId.value, { title: sessions.deriveTitle(trimmed) })
    timeline.value.push({ kind: 'user', id: nextId(), mid: '', content: trimmed })
    runState.value = 'thinking'
    transport.value?.send({ command: 'send_guide', key })
    persistSoon()
  }

  /** 时间线写回 localStorage 有节流：流式期间不要每个 chunk 都序列化。 */
  function persistSoon() {
    if (isDemoMode()) return // 演示内容不该混进真实历史
    if (persistTimer) return
    persistTimer = setTimeout(() => {
      persistTimer = null
      const items = timeline.value.filter(t => t.kind !== 'notice')
      if (items.length === 0) return
      sessions.touch(sessionId.value, { turns: timeline.value.filter(t => t.kind === 'user').length })
      sessions.saveTranscript(sessionId.value, timeline.value)
    }, 1200)
  }

  function flushPersistence() {
    if (persistTimer) {
      clearTimeout(persistTimer)
      persistTimer = null
    }
    if (isDemoMode()) return
    const items = timeline.value.filter(t => t.kind !== 'notice')
    if (items.length === 0) return
    sessions.touch(sessionId.value, { turns: timeline.value.filter(t => t.kind === 'user').length })
    sessions.saveTranscript(sessionId.value, timeline.value)
  }

  /** 换 sessionId 后必须重连：WS 的路径里带着 session id。 */
  function connect(wsUrl?: string) {
    // 并行会话：目标会话已有活跃连接则复用（不打断后台任务），否则新建。
    // 这是并发与「切换不崩」的关键：绝不能 close 还在跑任务的旧连接。
    const targetSessionId = sessionId.value
    if (!isDemoMode()) void restoreFromEngine(targetSessionId)
    const existing = transportPool.get(targetSessionId)
    if (existing) {
      transport.value = existing
      connectedSessionId = targetSessionId
      connection.setStatus({ state: 'open' })
      existing.send({ command: 'set_permission_mode', mode: config.permissionMode })
      existing.send({ command: 'set_session_mode', mode: mode.value })
      const meta = sessions.find(targetSessionId)
      const providerId = meta?.providerId ?? config.currentProviderId
      const model = meta?.model ?? config.currentModel
      if (providerId && model) existing.send({ command: 'select_model', provider_id: providerId, model })
      if (!auxiliary) existing.send({ command: 'set_reasoning_effort', effort: config.reasoningEffort })
      if (!auxiliary) existing.send({ command: 'set_max_tool_rounds', rounds: config.maxToolRounds })
      return
    }
    if (wsUrl) connection.setWsUrl(wsUrl)
    // 池满：关闭最久未用的连接（Map 迭代顺序 = 插入顺序，第一个即最老）。
    while (transportPool.size >= TRANSPORT_POOL_MAX) {
      const oldest = transportPool.keys().next().value as string | undefined
      if (!oldest || oldest === targetSessionId) break
      const oldT = transportPool.get(oldest)
      transportPool.delete(oldest)
      oldT?.close?.()
    }
    const t = createTransport(targetSessionId, wsUrl)
    transportPool.set(targetSessionId, t)
    transport.value = t
    connectedSessionId = targetSessionId
    let opened = false
    t.onStateChange(async status => {
      if (transport.value !== t || sessionId.value !== targetSessionId) return
      connection.setStatus(status)
      if (status.state === 'open') {
        if (opened && !isDemoMode()) void restoreFromEngine(targetSessionId)
        opened = true
        await config.syncStartupPermission()
        if (transport.value !== t || sessionId.value !== targetSessionId) return
        t.send({ command: 'set_permission_mode', mode: config.permissionMode })
        t.send({ command: 'set_session_mode', mode: mode.value })
        const meta = sessions.find(targetSessionId)
        const providerId = meta?.providerId ?? config.currentProviderId
        const model = meta?.model ?? config.currentModel
        if (providerId && model) {
          t.send({ command: 'select_model', provider_id: providerId, model })
        }
        if (!auxiliary) t.send({ command: 'set_reasoning_effort', effort: config.reasoningEffort })
        if (!auxiliary) t.send({ command: 'set_max_tool_rounds', rounds: config.maxToolRounds })
      }
    })
    t.onMessage(env => {
      const isForeground = sessionId.value === targetSessionId
      if (env.type === 'event') {
        const seq = env.payload.event_seq
        if (isForeground && env.payload.event_type === 'turn_truncated') {
          // A reset can lower the sequence number. Always refresh from durable
          // history, including when another window initiated the undo.
          if (!undoConfirm.value?.busy) void restoreFromEngine(targetSessionId)
        }
        else if (isForeground && restoring.value) restoreEvents.push(env.payload)
        else if (isForeground && (typeof document === 'undefined' || !document.hidden) && (!seq || seq > (appliedThrough.get(targetSessionId) ?? 0))) {
          onInbound(env)
          if (seq) appliedThrough.set(targetSessionId, seq)
        }
        // Acknowledgements are transport-only. The engine's view journal still
        // contains background events when this store later requests a snapshot.
        if (seq) t.send({ command: 'ack_event', event_seq: seq })
      } else if (isForeground && env.type === 'error') onInbound(env)
    })
    t.connect()
  }

  function disconnect() {
    for (const t of transportPool.values()) t?.close?.()
    transportPool.clear()
    transport.value = null
    connectedSessionId = ''
  }

  /** Recreate the active socket so newly saved retry settings take effect immediately. */
  function reconnect() {
    const current = sessionId.value
    const previous = transportPool.get(current)
    transportPool.delete(current)
    previous?.close()
    transport.value = null
    connectedSessionId = ''
    connect(connection.wsUrl || undefined)
  }

  function onInbound(env: InboundEnvelope) {
    if (env.type === 'event') applyEvent(env.payload)
    else if (env.type === 'error') pushNotice('error', env.payload.message)
  }

  function applyEvent(ev: AgentEvent) {
    const before = timeline.value.length
    switch (ev.event_type) {
      case 'assistant_message': {
        if (ev.reasoning) {
          const existing = timeline.value.find(item => item.kind === 'reasoning' && item.mid === ev.message_id)
            ?? [...timeline.value].reverse().find(item => item.kind === 'reasoning' && !item.mid)
          if (existing?.kind === 'reasoning') { existing.mid = ev.message_id; existing.content = ev.reasoning; existing.streaming = false; existing.state = ev.reasoning_state }
          else timeline.value.push({ kind: 'reasoning', id: nextId(), mid: ev.message_id, content: ev.reasoning, streaming: false, state: ev.reasoning_state, expanded: false })
        }
        if (!ev.content) break
        let item = timeline.value.find((item): item is AssistantMessage => item.kind === 'assistant' && item.mid === ev.message_id)
        if (!item) item = [...timeline.value].reverse().find((item): item is AssistantMessage => item.kind === 'assistant' && !item.mid)
        if (item) { item.mid = ev.message_id; item.content = ev.content }
        else timeline.value.push({ kind: 'assistant', id: nextId(), mid: ev.message_id, content: ev.content, streaming: false })
        endAssistantStream()
        break
      }
      case 'user_message': {
        if (timeline.value.some(item => item.kind === 'user' && item.mid === ev.message_id)) break
        const parsed = parseAttachmentRequest(ev.content)
        const optimistic = timeline.value.find((item): item is UserMessage => item.kind === 'user' && !item.mid && item.content === parsed.text)
          ?? timeline.value.find((item): item is UserMessage => item.kind === 'user' && !item.mid)
        if (optimistic) optimistic.mid = ev.message_id
        else {
          endAssistantStream()
          timeline.value.push({ kind: 'user', id: nextId(), mid: ev.message_id, content: parsed.text, attachments: parsed.attachments })
        }
        break
      }
      // 兜底：turn_end 之后又开始吐字（引擎续了一轮），状态得跟着回到忙。
      case 'text_chunk': finishReasoning(); connection.setRetry(null); if (runState.value === 'idle') runState.value = 'thinking'; appendAssistant(ev.content); pushControlStatus('正在输出'); break
      case 'reasoning_end': finishReasoning(); break
      case 'reasoning_chunk':
        if (runState.value === 'idle') runState.value = 'thinking'
        appendReasoning(ev.content)
        // 控制模式：思考片段同步到桌面悬浮层。用户此时多半已经切到目标 App，
        // 悬浮层是他唯一能看到「它在想什么」的地方。
        pushControlStatus('正在思考')
        queueFloatReasoning(ev.content)
        break
      case 'plan_updated': {
        plan.value = {
          active: true,
          steps: (ev.steps ?? []).map(s => ({ step: s.step, status: s.status })),
          explanation: ev.explanation,
        }
        break
      }
      case 'tool_start':
        connection.setRetry(null)
        endAssistantStream()
        timeline.value.push({ kind: 'tool', callId: ev.call_id, toolName: ev.tool_name, arguments: ev.arguments, status: 'starting', expanded: ev.tool_name === 'show_image' })
        flushFloatReasoning()
        pushControlStatus(`正在调用 ${ev.tool_name}`)
        // summarizeArguments 返回的是结构化的摘要对象，转发给悬浮层要转成一行文字
        pushTrace(`调用工具 ${ev.tool_name}`, describeFloatArguments(ev.arguments))
        turnToolTrace.push({
          callId: ev.call_id,
          sequence: turnToolTrace.length + 1,
          tool: sanitizeToolName(ev.tool_name),
          argumentShape: summarizeArguments(ev.arguments),
          status: 'running',
        })
        runState.value = 'executing'
        break
      case 'tool_running': patchTool(ev.call_id, c => c.status = 'running'); runState.value = 'executing'; break
      case 'tool_done':
        patchTool(ev.call_id, c => {
          c.status = ev.is_error ? 'error' : 'success'
          c.elapsed = ev.elapsed
          c.resultPreview = ev.result_preview
          c.isError = ev.is_error
          // 工具产生的图片：瀑布流渲染（历史恢复时由 messages.images 补回）
          if (Array.isArray(ev.images) && ev.images.length > 0) c.images = ev.images
        })
        // 控制模式：工具结果也送到悬浮层。用户切到目标 App 后，这是唯一能看到
        // 「它刚才做的事成没成」的地方。
        pushTrace(
          `${ev.is_error ? '✕' : '✓'} ${ev.tool_name} · ${ev.elapsed.toFixed(1)}s`,
          ev.result_preview,
        )
        pushControlStatus(ev.is_error ? '工具失败，正在调整' : '正在继续')
        // 工具跑完不等于一轮结束 —— 模型接着想下一步。回 idle 只认 turn_end /
        // 取消 / 致命错误，否则输入区会在循环中途闪回「下达任务」和发送箭头。
        runState.value = 'thinking'
        {
          const trace = turnToolTrace.find(item => item.callId === ev.call_id)
          if (trace) {
            trace.status = ev.is_error ? 'error' : 'success'
            trace.elapsedMs = Math.max(0, Math.round(ev.elapsed * 1000))
            if (ev.is_error) {
              consecutiveToolFailures += 1
              maxConsecutiveToolFailures = Math.max(maxConsecutiveToolFailures, consecutiveToolFailures)
              trace.category = classifyToolError(ev.result_preview)
              trace.errorSummary = sanitizeDiagnosticText(ev.result_preview)
              if (maxConsecutiveToolFailures >= 3 && !failureNoticeCreated) {
                failureNoticeCreated = true
                const noticeId = nextId()
                timeline.value.push({
                    kind: 'notice', id: noticeId, tone: 'warn',
                    text: `同一任务链连续 ${maxConsecutiveToolFailures} 次工具调用未恢复，请检查工具参数或环境。`,
                })
              }
            } else consecutiveToolFailures = 0
          }
        }
        break
      case 'tool_cache_hit':
        patchTool(ev.call_id, c => c.status = 'cache_hit')
        {
          const trace = turnToolTrace.find(item => item.callId === ev.call_id)
          if (trace) trace.status = 'success'
          consecutiveToolFailures = 0
        }
        break
      case 'tool_approval_request':
        endAssistantStream()
        if (!patchTool(ev.call_id, c => { c.status = 'awaiting_approval'; c.access = ev.access; c.riskSummary = ev.risk_summary; c.expanded = true })) {
          timeline.value.push({ kind: 'tool', callId: ev.call_id, toolName: ev.tool_name, arguments: ev.arguments, status: 'awaiting_approval', access: ev.access, riskSummary: ev.risk_summary, expanded: true })
        }
        runState.value = 'awaiting_approval'
        break
      case 'user_question_request':
        endAssistantStream()
        timeline.value.push({ kind: 'question', callId: ev.call_id, questions: ev.questions, answered: false })
        runState.value = 'awaiting_question'
        break
      case 'file_transfer_request':
        if (handledFileRequests.has(ev.request_id)) break
        handledFileRequests.add(ev.request_id)
        if (auxiliary) auxiliaryTransfers.set(ev.request_id, paths => completeFileTransfer(ev.request_id, paths))
        if (ev.operation === 'import') {
          window.CoomiAndroid?.importFilesForRequest?.(ev.request_id)
        } else if (ev.path) {
          window.CoomiAndroid?.exportFileForRequest?.(
            ev.request_id,
            ev.path,
            ev.suggested_name ?? ev.path.split('/').pop() ?? 'coomi-export',
          )
        }
        break
      case 'usage_update': {
        const previous = usage.value
        usage.value = {
          total: ev.usage.total_tokens ?? previous?.total ?? 0,
          input: ev.usage.input_tokens ?? previous?.input ?? 0,
          output: ev.usage.output_tokens ?? previous?.output ?? 0,
          contextRatio: ev.usage.context_ratio ?? previous?.contextRatio ?? 0,
          contextUsed: ev.usage.context_used_tokens ?? previous?.contextUsed ?? 0,
          contextWindow: ev.usage.context_window_tokens ?? previous?.contextWindow ?? 0,
          cachedInput: ev.usage.cached_input_tokens ?? previous?.cachedInput ?? 0,
          cacheHitRate: ev.usage.cache_hit_rate ?? previous?.cacheHitRate ?? null,
          cacheDataAvailable: ev.usage.cache_data_available ?? previous?.cacheDataAvailable ?? false,
          turnCacheHitRate: ev.usage.turn_cache_hit_rate ?? previous?.turnCacheHitRate ?? null,
          turnCacheDataAvailable: ev.usage.turn_cache_data_available ?? previous?.turnCacheDataAvailable ?? false,
          turnRateTps: ev.usage.turn_rate_tps ?? previous?.turnRateTps ?? 0,
          turnElapsedMs: ev.usage.turn_elapsed_ms ?? previous?.turnElapsedMs ?? 0,
          turnOutputTokens: ev.usage.turn_output_tokens ?? previous?.turnOutputTokens ?? 0,
          reasoningEfforts: ev.reasoning_efforts ?? previous?.reasoningEfforts ?? {},
          contextCategories: ev.context_categories ?? previous?.contextCategories ?? {},
        }
        if (ev.usage.total_tokens != null) {
          sessions.updateTokens(sessionId.value, ev.usage.total_tokens)
        }
        break
      }
      case 'compression_started': pushNotice('info', ev.automatic ? '正在自动压缩上下文…' : '正在手动压缩上下文，完整历史将保留…'); break
      case 'compression': {
        const id = ev.summary_id ? `compression:${ev.summary_id}` : nextId()
        if (!timeline.value.some(item => 'id' in item && item.id === id)) {
          timeline.value.push({ kind: 'notice', id, tone: 'success', text: `上下文已压缩 ${fmtTokens(ev.before)} → ${fmtTokens(ev.after)}`, detail: ev.summary })
        }
        break
      }
      case 'connection_retry': connection.setRetry(`${ev.message}（${ev.attempt}/${ev.max_attempts}）`); break
      case 'stream_reset':
        endAssistantStream()
        while (timeline.value.length > 0) {
          const last = timeline.value[timeline.value.length - 1]
          if ((last.kind === 'assistant' || last.kind === 'reasoning') && !last.mid) timeline.value.pop()
          else break
        }
        break
      case 'retry_confirmation':
        endAssistantStream()
        runState.value = 'idle'
        retryConfirmation.value = ev.message
        break
      case 'agent_error': {
        endAssistantStream(); pushNotice('error', ev.message); if (ev.is_fatal) runState.value = 'idle'; persistSoon()
        // 账号登录类 Provider（DeepSeek / 智谱 Coding Plan）已不在公开源码中提供入口
        if ((ev as any).captcha_required) {
          pushNotice('warn', '该模型需要账号登录验证，请在本地版本中使用')
        }
        break
      }
      case 'configuration_required': endAssistantStream(); runState.value = 'idle'; pushNotice('warn', ev.message); if (!auxiliary && !replaying) void router.push(ev.route); break
      case 'agent_cancelled': endAssistantStream(); cancelRunningTools(); pushNotice('warn', '已停止本轮执行'); break
      case 'bg_task_detached': pushNotice('info', `↪ 已转入后台任务 #${ev.task_id}（${ev.tool_name}）`); break
      case 'bg_task_completed': pushNotice(ev.is_error ? 'error' : 'success', `${ev.is_error ? '✕' : '✓'} 后台任务 #${ev.task_id} ${ev.is_error ? '失败' : '完成'}`); break
      case 'loop_progress':
        loop.value = { active: ev.status !== 'done', currentStep: ev.current_step, totalSteps: ev.total_steps, status: ev.status, currentDescription: loop.value.currentDescription }
        break
      case 'loop_step_start':
        loop.value = { ...loop.value, active: true, totalSteps: ev.total_steps, currentStep: ev.step_index, currentDescription: ev.step_description }
        break
      case 'life_delivered': {
        // 气泡投递完成：把最后一条 assistant 标记为生命体气泡并复位投递态。
        lifeDelivering.value = false
        const last = timeline.value[timeline.value.length - 1]
        if (last?.kind === 'assistant') last.life = true
        void refreshLifeUnread()
        break
      }
      case 'turn_end':
        endAssistantStream(); cancelRunningTools(); connection.setRetry(null); runState.value = 'idle'
        plan.value = { active: false, steps: [] }
        {
          const failures = turnToolTrace.filter(item => item.status === 'error').length
          if (maxConsecutiveToolFailures >= 3 && !failureNoticeCreated) {
            const trace = turnToolTrace.map(item => ({ ...item, callId: undefined }))
            const noticeId = nextId()
            timeline.value.push({
              kind: 'notice', id: noticeId, tone: 'warn',
              text: `同一任务链连续 ${maxConsecutiveToolFailures} 次工具调用未恢复，请检查工具参数或环境。`,
            })
          }
        }
        turnToolTrace = []
        consecutiveToolFailures = 0
        maxConsecutiveToolFailures = 0
        failureNoticeCreated = false
        persistSoon()
        break
      case 'session_state': {
        // 重连后引擎告知本会话是否仍在后台执行（切走会话后任务继续跑）。
        sessions.refreshRunning()
        runState.value = ev.running ? 'thinking' : 'idle'
        break
      }
      case 'session_loaded': {
        // 打开历史会话时，引擎把持久化的累计用量推过来，避免显示 0。
        const u = ev.usage ?? {}
        usage.value = {
          total: u.total_tokens ?? usage.value?.total ?? 0,
          input: u.input_tokens ?? usage.value?.input ?? 0,
          output: u.output_tokens ?? usage.value?.output ?? 0,
          contextRatio: usage.value?.contextRatio ?? 0,
          contextUsed: usage.value?.contextUsed ?? 0,
          contextWindow: usage.value?.contextWindow ?? 0,
          cachedInput: usage.value?.cachedInput ?? 0,
          cacheHitRate: usage.value?.cacheHitRate ?? null,
          cacheDataAvailable: usage.value?.cacheDataAvailable ?? false,
          turnCacheHitRate: usage.value?.turnCacheHitRate ?? null,
          turnCacheDataAvailable: usage.value?.turnCacheDataAvailable ?? false,
          turnRateTps: usage.value?.turnRateTps ?? 0,
          turnElapsedMs: usage.value?.turnElapsedMs ?? 0,
          turnOutputTokens: usage.value?.turnOutputTokens ?? 0,
          reasoningEfforts: usage.value?.reasoningEfforts ?? {},
          contextCategories: usage.value?.contextCategories ?? {},
        }
        if (u.total_tokens != null) {
          sessions.updateTokens(sessionId.value, u.total_tokens)
        }
        if (typeof ev.cwd === 'string' && ev.cwd) cwd.value = ev.cwd
        break
      }
    }
    // The durable user message defines the turn boundary in both live and restored views.
    const user = [...timeline.value].reverse().find(item => item.kind === 'user')
    const turnId = user?.turnId || (user && 'id' in user ? user.id : '')
    for (let i = before; i < timeline.value.length; i++) timeline.value[i].turnId ||= turnId
  }

  function activateSession(id: string) {
    selectionEpoch += 1
    restoreAbort?.abort()
    restoring.value = !isDemoMode()
    sessionId.value = id
    mode.value = resolveLifeMode(id)
    // 顶栏模型显示跟随会话：每个会话保存自己的模型（sessions.setModel 记录在 meta）。
    // 这里只改「显示」，引擎侧真正的模型由 connect() 里按 meta 发的 select_model 生效。
    const meta = sessions.find(id)
    if (meta?.providerId && meta.model) {
      config.syncDisplayModel(meta.providerId, meta.model)
    }
    if (!auxiliary) persistActiveSessionId(id)
    lifeAutoSent = false
    if (!auxiliary) {
      try { window.CoomiAndroid?.setControlSession?.(id, config.currentProviderId, config.currentModel) } catch { /* bridge unavailable */ }
    }
  }

  /**
   * 会话模式决议：生命体人格只属于「常驻会话」；「用于全局会话」开关开启后所有会话都带人格。
   * 历史会话即使曾被切成 life，关闭全局开关后也强制回到 agent（人格只活在它该在的地方）。
   */
  function resolveLifeMode(id: string): 'agent' | 'life' {
    return !auxiliary && config.digitalLifeEnabled && (isGlobalSessionId(id) || config.lifeGlobalMode) ? 'life' : 'agent'
  }

  const isGlobalSession = computed(() => isGlobalSessionId(sessionId.value))

  function retryInterruptedTurn() {
    retryConfirmation.value = null
    runState.value = 'thinking'
    transport.value?.send({ command: 'retry_turn' })
  }

  function dismissRetry() { retryConfirmation.value = null }

  function setReasoningEffort(effort: ReasoningEffort) {
    config.setReasoningEffort(effort)
    transport.value?.send({ command: 'set_reasoning_effort', effort })
  }

  function setProductionMode(value: 'normal' | 'overload' | 'berserk') {
    config.setProductionMode(value)
    transport.value?.send({ command: 'set_production_mode', mode: value })
  }

  function setMaxToolRounds(rounds: number) {
    config.setMaxToolRounds(rounds)
    transport.value?.send({ command: 'set_max_tool_rounds', rounds: config.maxToolRounds })
  }

  function cancelRunningTools() {
    // 停止后引擎可能不会逐个补发 tool_done：把仍在运行/准备中的工具卡片
    // 收尾为「已取消」，否则卡片会永远停在旋转的「运行中」状态。
    let changed = false
    for (const item of timeline.value) {
      if (item.kind === 'tool' && (item.status === 'running' || item.status === 'starting')) {
        item.status = 'cancelled'
        item.isError = true
        changed = true
      }
    }
    if (changed) persistSoon()
  }

  function sendMessage(text: string, displayText?: string, morph = false, attachments?: ChatAttachment[]): string | null {
    if (restoring.value) return null
    const trimmed = buildAttachmentRequest(text, attachments ?? [])
    const visible = displayText?.trim() ?? text.trim()
    if (!trimmed) return null
    // 编辑覆盖模式：截断目标轮次（与引擎 edit_turn 行为一致），以新文本重新执行。
    const edit = pendingEdit.value
    if (edit) {
      pendingEdit.value = null
      let cutAt = -1
      if (edit.mid) cutAt = timeline.value.findIndex(t => t.kind === 'user' && t.mid === edit.mid)
      if (cutAt < 0) {
        for (let i = timeline.value.length - 1; i >= 0; i--) {
          if (timeline.value[i].kind === 'user') { cutAt = i; break }
        }
      }
      if (cutAt >= 0) timeline.value.splice(cutAt)
      const id = nextId()
      timeline.value.push({ kind: 'user', id, mid: '', content: visible, attachments, morphing: morph })
      runState.value = 'thinking'
      transport.value?.send({ command: 'edit_turn', msg_id: edit.mid, text: trimmed })
      persistSoon()
      return id
    }
    // 首条用户消息作为会话标题，抽屉里就不会全是「新对话」。
    const isFirst = !timeline.value.some(t => t.kind === 'user')
    if (isFirst) sessions.touch(sessionId.value, { title: sessions.deriveTitle(visible) })
    if (isBusy.value) {
      const id = nextId()
      timeline.value.push({ kind: 'user', id, mid: '', content: visible, attachments, morphing: morph })
      transport.value?.send({ command: 'jump_in', text: trimmed })
      persistSoon()
      return id
    }
    turnToolTrace = []
    const id = nextId()
    timeline.value.push({ kind: 'user', id, mid: '', content: visible, attachments, morphing: morph })
    runState.value = 'thinking'
    // 直接发送；如遇 3007 需验证码，引擎发 captcha_required 事件，前端跳转验证页处理
    transport.value?.send({ command: 'send_message', text: trimmed })
    persistSoon()
    return id
  }

  function completeSendMorph(id: string) {
    const message = timeline.value.find((item): item is UserMessage => item.kind === 'user' && item.id === id)
    if (!message || !message.morphing) return
    message.morphing = false
    message.morphArrived = true
    setTimeout(() => { message.morphArrived = false }, 1100)
  }

  function cancel() { transport.value?.send({ command: 'cancel' }) }
  function approve(callId: string, decision: 'allow' | 'deny' | 'always') {
    patchTool(callId, c => { c.status = decision === 'deny' ? 'error' : 'running'; if (decision === 'deny') { c.resultPreview = '（用户拒绝执行）'; c.isError = true } })
    transport.value?.send({ command: 'approve_tool', call_id: callId, decision })
    if (runState.value === 'awaiting_approval') runState.value = 'executing'
  }
  function answerQuestion(callId: string, answers: Record<string, string>) {
    patchQuestion(callId, q => { q.answered = true; q.answers = answers })
    transport.value?.send({ command: 'answer_question', call_id: callId, answers })
    if (runState.value === 'awaiting_question') runState.value = 'thinking'
  }
  function setPermissionMode(mode: 'ask' | 'auto' | 'full' | 'minimal') { config.setPermissionMode(mode); transport.value?.send({ command: 'set_permission_mode', mode }) }
  function togglePlanMode() { const entering = !config.planMode; config.togglePlanMode(); transport.value?.send({ command: entering ? 'enter_plan_mode' : 'exit_plan_mode' }) }
  async function selectModel(providerId: string, model: string) {
    if (!auxiliary && !(await config.validateAndSelectModel(providerId, model))) {
      pushNotice('error', config.lastError || '模型凭据验证失败，未切换模型')
      return
    }
    transport.value?.send({ command: 'select_model', provider_id: providerId, model })
    sessions.setModel(sessionId.value, providerId, model)
  }
  function setSessionMode(value: 'agent' | 'life') {
    if (isBusy.value || mode.value === value) return
    mode.value = value
    sessions.setMode(sessionId.value, value)
    transport.value?.send({ command: 'set_session_mode', mode: value })
  }
  /** 按「人格只属于常驻会话 / 用于全局会话开关」重算当前会话模式并同步引擎。 */
  function syncLifeMode() {
    if (isBusy.value) return
    const next = resolveLifeMode(sessionId.value)
    if (mode.value === next) return
    mode.value = next
    sessions.setMode(sessionId.value, next)
    transport.value?.send({ command: 'set_session_mode', mode: next })
  }
  function completeFileTransfer(requestId: string, paths: string[]) {
    transport.value?.send({ command: 'file_transfer_result', request_id: requestId, paths })
  }

  /** 生命体未读问候（/api/life/unread）：窗口轮询 + 打开会话时刷新。 */
  async function refreshLifeUnread() {
    try {
      const data = await apiGet<{ pending: LifeUnreadItem | null; lifeName?: string } | null>('/api/life/unread')
      const pending = data?.pending ?? null
      lifeUnread.value = pending ? [pending] : []
      lifeUnreadName.value = pending?.lifeName ?? ''
      if (!pending) {
        // 引擎侧已无未读（可能已投递/过期）：复位投递态，允许后续再触发。
        lifeDelivering.value = false
        lifeAutoSent = true
      }
    } catch {
      /* 引擎未就绪时保持上次状态 */
    }
  }

  /** 手动投递（气泡 pill）：引擎侧把队列第一条写入本会话并流式推送。 */
  function deliverLife() {
    if (!lifeUnread.value.length || lifeDelivering.value) return
    lifeDelivering.value = true
    lifeAutoSent = true
    transport.value?.send({ command: 'deliver_life' })
  }

  /**
   * 开场问候：常驻会话已打开且引擎已连、有未读时自动投递一次。
   * 由 ChatView 常驻轮询每 2s 调用；lifeDelivering/lifeAutoSent 防止重复触发。
   * 主动消息只在常驻会话出现，其他会话永不投递。
   */
  function autoDeliverLifeIfReady() {
    if (lifeAutoSent || lifeDelivering.value) return
    if (!isGlobalSessionId(sessionId.value)) return
    if (mode.value !== 'life' || !lifeUnread.value.length || isBusy.value) return
    if (!connection.isOpen) return
    deliverLife()
    void refreshLifeUnread()
  }

  function newSession() {
    flushPersistence()
    endAssistantStream(); timeline.value = []; usage.value = null
    loop.value = { active: false, currentStep: 0, totalSteps: 0, status: '' }; runState.value = 'idle'
    plan.value = { active: false, steps: [] }
    pendingEdit.value = null
    undoConfirm.value = null
    activateSession(createSessionId())
    connect()
  }

  /** Restore one coherent view, then apply events newer than its watermark. */
  function restoreFromEngine(id: string): Promise<boolean> {
    const epoch = selectionEpoch
    if (pendingRestore?.id === id && pendingRestore.epoch === epoch) return pendingRestore.promise
    restoreAbort?.abort()
    const controller = new AbortController()
    restoreAbort = controller
    const serial = ++restoreSerial
    restoring.value = true
    const active = () => serial === restoreSerial && epoch === selectionEpoch && id === sessionId.value
    const promise = (async () => {
      try {
        for (let attempt = 0; attempt < 3; attempt++) {
          restoreEvents = new RecoveryEvents<AgentEvent>()
          const timeout = setTimeout(() => controller.abort(), 15000)
          let res: Response
          try { res = await authedFetch(`/api/sessions/${id}/snapshot`, { signal: controller.signal }) }
          finally { clearTimeout(timeout) }
          if (!active()) return false
          let snapshot: SessionSnapshot
          if (res.status === 404) {
            // Compatibility with an engine installed before the snapshot API.
            res = await authedFetch(`/api/sessions/${id}`, { signal: controller.signal })
            if (!res.ok) return false
            snapshot = { session: await res.json(), events: [], event_seq: 0, running: false }
          } else {
            if (!res.ok) return false
            const data = await res.json()
            snapshot = data.session ? data : { session: data, events: [], event_seq: 0, running: false }
          }
          if (!active()) return false
          if (restoreEvents.overflow) continue
          endAssistantStream()
          usage.value = null
          retryConfirmation.value = null
          turnToolTrace = []; consecutiveToolFailures = 0; maxConsecutiveToolFailures = 0; failureNoticeCreated = false
          loop.value = { active: false, currentStep: 0, totalSteps: 0, status: '' }
          plan.value = { active: false, steps: [] }
          const session = snapshot.session
          const messages = session.messages ?? []
          const archive = session.archive ?? []
          const merged = archive.length ? [...archive, ...messages.filter(m => !m.compaction_summary && !archive.some(a => a.id && a.id === m.id))] : messages
          timeline.value = messagesToTimeline(merged)
          if (archive.length) {
            timeline.value.push({ kind: 'notice', id: `compression:${messages.find(m => m.compaction_summary)?.id}`, tone: 'info', text: '（上下文已压缩 · 完整历史已保留）', detail: messages.find(m => m.compaction_summary)?.content ?? '' })
          }
          mode.value = resolveLifeMode(id)
          sessions.setMode(id, mode.value)
          replaying = true
          try {
            applyEvent({ event_type: 'session_loaded', session_id: id, cwd: session.cwd ?? '', usage: { input_tokens: session.usage?.input_tokens ?? 0, output_tokens: session.usage?.output_tokens ?? 0, total_tokens: session.usage?.total_tokens ?? 0 } })
            for (const event of snapshot.events ?? []) applyEvent(event)
            if (!snapshot.running) { endAssistantStream(); cancelRunningTools(); runState.value = 'idle' }
            else if (pendingApproval.value) runState.value = 'awaiting_approval'
            else if (pendingQuestion.value) runState.value = 'awaiting_question'
            else runState.value = 'thinking'
            const watermark = snapshot.event_seq ?? 0
            appliedThrough.set(id, watermark)
            for (const event of restoreEvents.after(watermark)) {
              applyEvent(event)
              appliedThrough.set(id, event.event_seq!)
            }
            if (snapshot.truncated) pushNotice('info', '运行中内容过长，部分过程预览已省略；完整结果以会话记录为准。')
          } finally { replaying = false }
          return true
        }
        throw new Error('恢复期间事件过多，请重新打开会话')
      } catch (error) {
        if (active() && !controller.signal.aborted) console.warn('session restore failed', error)
        return false
      } finally {
        if (active()) {
          restoring.value = false
          viewRevision.value += 1
          pendingRestore = null
          persistSoon()
        }
      }
    })()
    pendingRestore = { id, epoch, promise }
    return promise
  }

  function resumeFromBackground() {
    if ((typeof document === 'undefined' || !document.hidden) && !isDemoMode()) void restoreFromEngine(sessionId.value)
  }

  /** 把引擎磁盘会话消息转换为前端时间线（含工具调用卡片与结果回填）。 */
  function messagesToTimeline(messages: ChatMessageJson[]): Timelineitem[] {
    const items: Timelineitem[] = []
    let turnId = ''
    const toolResults = new Map<string, string>()
    const toolImages = new Map<string, string[]>()
    for (const m of messages) {
      if (m.internal) continue
      if (m.compaction_summary) {
        items.push({ kind: 'notice', id: `compression:${m.id}`, tone: 'info', text: '（上下文已压缩 · 点击查看摘要）', detail: m.content ?? '' })
        continue
      }
      const start = items.length
      if (m.role === 'user') {
        turnId = m.id || nextId()
        const parsed = parseAttachmentRequest(m.content)
        items.push({ kind: 'user', id: nextId(), mid: m.id ?? '', content: parsed.text, attachments: parsed.attachments })
      } else if (m.role === 'assistant') {
        if (m.reasoning) items.push({ kind: 'reasoning', id: `reasoning:${m.id || nextId()}`, mid: m.id, content: m.reasoning, expanded: false, streaming: false, state: m.reasoning_state })
        if (m.content) items.push({ kind: 'assistant', id: nextId(), mid: m.id ?? '', content: m.content, streaming: false, life: m.life_proactive === true })
        for (const tc of m.tool_calls ?? []) {
          items.push({
            kind: 'tool', callId: tc.id, toolName: tc.name,
            arguments: tc.arguments as Record<string, unknown>,
            status: 'success', expanded: tc.name === 'show_image',
            images: (tc.images ?? []).map((img: { media_type: string; data: string }) =>
              `data:${img.media_type};base64,${img.data}`),
          })
        }
      } else if (m.role === 'tool' && m.tool_call_id) {
        toolResults.set(m.tool_call_id, m.content)
        if (m.images?.length) {
          toolImages.set(m.tool_call_id, m.images.map((img: { media_type: string; data: string }) =>
            `data:${img.media_type};base64,${img.data}`))
        }
      }
      for (let i = start; i < items.length; i++) items[i].turnId = turnId
    }
    for (const item of items) {
      if (item.kind === 'tool') {
        const result = toolResults.get(item.callId)
        if (result != null) {
          const preview = result.length > 200 ? result.slice(0, 200) + '…' : result
          item.resultPreview = preview
          item.isError = /error|fail|exception|panic/i.test(result.slice(0, 500))
        } else {
          // 没有结果回填（比如被取消/未执行）的调用收尾为已取消
          item.status = 'cancelled'
          item.isError = true
        }
        const imgs = toolImages.get(item.callId)
        if (imgs?.length) {
          item.images = imgs
        } else if (item.toolName === 'show_image' && item.expanded && item.status === 'success') {
          // show_image 历史恢复但图片数据不可用（如已被上下文压缩清理）
          item.imageMissing = true
        }
      }
    }
    return items
  }

  /**
   * 打开一条历史会话：优先从引擎磁盘拉完整历史（权威源，修复“会话消失/串话”），
   * 引擎不可用才回退本机 localStorage 记录。
   */
  async function openSession(id: string) {
    if (id === sessionId.value) { resumeFromBackground(); return }
    flushPersistence()
    endAssistantStream()
    usage.value = null
    loop.value = { active: false, currentStep: 0, totalSteps: 0, status: '' }
    plan.value = { active: false, steps: [] }
    pendingEdit.value = null
    undoConfirm.value = null
    runState.value = 'idle'
    const targetId = isUuid(id) ? id : sessions.migrateId(id, createSessionId())
    activateSession(targetId)
    const epoch = selectionEpoch
    timeline.value = []
    connect()
    if (isDemoMode()) { restoring.value = false; return }
    const restored = await restoreFromEngine(targetId)
    if (epoch !== selectionEpoch || sessionId.value !== targetId) return
    if (!restored) timeline.value = sessions.loadTranscript(targetId)
  }

  function deleteSession(id: string) {
    // 先停掉待落盘的持久化定时器：被删会话不应再写回（否则会“复活”成空标题的新会话）。
    if (persistTimer) { clearTimeout(persistTimer); persistTimer = null }
    if (id === sessionId.value) {
      // 删除的是当前会话：先切到新会话并重连（关闭旧 id 的 WS 连接、清空时间线），
      // 避免 flushPersistence 把已删会话写回，也避免引擎在文件删除后重建同 id 会话。
      endAssistantStream(); timeline.value = []; usage.value = null
      loop.value = { active: false, currentStep: 0, totalSteps: 0, status: '' }; runState.value = 'idle'
      plan.value = { active: false, steps: [] }
      activateSession(createSessionId())
      connect()
    }
    sessions.remove(id)
    try { localStorage.removeItem(`coomi.draft.${id}`) } catch { /* ignore */ }
  }

  /** 更新当前会话的工作目录（会话标记路径）。成功后引擎后续 turn 都在该目录执行。 */
  async function setSessionCwd(path: string): Promise<boolean> {
    const id = sessionId.value
    try {
      const res = await authedFetch(`/api/sessions/${id}/cwd`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ cwd: path }),
      })
      if (!res.ok) return false
      cwd.value = path
      const meta = sessions.find(id)
      if (meta) { meta.cwd = path; sessions.setCurrentCwd(path) }
      return true
    } catch {
      return false
    }
  }

  /** 编辑单条消息正文（改文本），成功后从引擎重新拉取时间线。 */
  /** 进入编辑模式：把旧文本回填到输入框，发送时覆盖该轮重新执行。 */
  function startEditMessage(mid: string, content: string, attachments: ChatAttachment[] = []) {
    if (isBusy.value) return
    pendingEdit.value = { mid, content }
    window.dispatchEvent(new CustomEvent('coomi:prefill-draft', {
      detail: { sessionId: sessionId.value, text: content, attachments },
    }))
  }

  /** 取消编辑模式（输入框内容保留，可继续作为普通新消息发送）。 */
  function cancelEditMessage() {
    pendingEdit.value = null
  }

  /** 点击「回撤」：先弹确认，避免误触后无法找回。 */
  let undoRequest = 0
  async function requestUndo(mid: string) {
    if (isBusy.value || !mid) return
    const request = ++undoRequest
    const target = { mid, sessionId: sessionId.value, busy: true, error: '' }
    undoConfirm.value = target
    try {
      const preview = await apiGet<NonNullable<NonNullable<typeof undoConfirm.value>['preview']>>(`/api/sessions/${target.sessionId}/messages/${encodeURIComponent(mid)}/undo`)
      if (request === undoRequest && undoConfirm.value?.mid === mid && sessionId.value === target.sessionId) Object.assign(undoConfirm.value, {preview, busy: false})
    } catch (e) {
      if (request === undoRequest && undoConfirm.value?.mid === mid && sessionId.value === target.sessionId) Object.assign(undoConfirm.value, {busy:false,error:e instanceof Error ? e.message : String(e)})
    }
  }

  /** 确认回撤：执行截断，返回选定轮之前的状态。 */
  async function confirmUndo(restoreFiles = false) {
    const target = undoConfirm.value
    if (!target?.preview || target.busy || target.sessionId !== sessionId.value) return
    target.busy = true
    target.error = ''
    try {
      await apiSend(`/api/sessions/${target.sessionId}/messages/${encodeURIComponent(target.mid)}/undo`, 'POST', {
        turn_id: target.preview.turn_id, revision: target.preview.revision, restore_files: restoreFiles,
      })
      if (target.sessionId !== sessionId.value) return
      undoConfirm.value = null
      pendingEdit.value = null
      if (!await restoreFromEngine(target.sessionId)) pushNotice('error', '回撤已保存，但界面刷新失败，请重新打开会话。')
    } catch (e) {
      if (target.sessionId !== sessionId.value) return
      target.busy = false
      target.error = e instanceof Error ? e.message : String(e)
    }
  }

  function cancelUndo() {
    if (undoConfirm.value?.busy) return
    undoConfirm.value = null
  }

  /** 回撤一轮：截断该轮 user 提问及其后的所有消息（含工具执行过程），不重新执行。 */
  function undoTurn(mid: string) {
    void requestUndo(mid)
  }

  function appendAssistant(content: string) {
    if (!currentAssistant) {
      timeline.value.push({ kind: 'assistant', id: nextId(), mid: '', content: '', streaming: true })
      // 必须拿 push 之后数组里的那个对象：ref 会把它包成代理，
      // 直接改 push 进去的原始对象不触发渲染，流式文本就只会停在第一片。
      currentAssistant = timeline.value[timeline.value.length - 1] as AssistantMessage
    }
    currentAssistant.content += content
  }
  function endAssistantStream() { finishReasoning(); if (currentAssistant) { currentAssistant.streaming = false; currentAssistant = null } }
  function appendReasoning(content: string) {
    const last = timeline.value[timeline.value.length - 1]
    if (last && last.kind === 'reasoning') { (last as ReasoningBlock).content += content }
    else { timeline.value.push({ kind: 'reasoning', id: nextId(), content, expanded: false, streaming: true }) }
  }
  function finishReasoning() {
    for (const item of timeline.value) if (item.kind === 'reasoning' && item.streaming) item.streaming = false
  }
  function patchTool(callId: string, fn: (c: ToolCard) => void): boolean {
    for (let i = timeline.value.length - 1; i >= 0; i--) { const t = timeline.value[i]; if (t.kind === 'tool' && t.callId === callId) { fn(t); return true } }
    return false
  }
  function patchQuestion(callId: string, fn: (q: QuestionCard) => void) {
    for (let i = timeline.value.length - 1; i >= 0; i--) { const t = timeline.value[i]; if (t.kind === 'question' && t.callId === callId) { fn(t); return } }
  }
  function pushNotice(tone: 'info' | 'warn' | 'error' | 'success', text: string) { timeline.value.push({ kind: 'notice', id: nextId(), tone, text }) }

  function toggleLifeStats() { lifeStatsOpen.value = !lifeStatsOpen.value }

  return { restoring, viewRevision, resumeFromBackground, sessionId, mode, timeline, runState, usage, plan, retryConfirmation, cwd, loop, isBusy, pendingEdit, undoConfirm, lastUserMessage, lastAssistantMessage, pendingApproval, pendingQuestion, lifeUnread, lifeUnreadName, lifeStatsOpen, toggleLifeStats, lifeDelivering, isGlobalSession, resolveLifeMode, syncLifeMode, refreshLifeUnread, deliverLife, autoDeliverLifeIfReady, connect, reconnect, disconnect, flushPersistence, sendMessage, completeSendMorph, cancel, approve, answerQuestion, setPermissionMode, setReasoningEffort, setProductionMode, setMaxToolRounds, setSessionMode, togglePlanMode, selectModel, retryInterruptedTurn, dismissRetry, completeFileTransfer, newSession, openSession, deleteSession, setSessionCwd, startEditMessage, cancelEditMessage, requestUndo, confirmUndo, cancelUndo, undoTurn, sendGuide }
  })
}

function fmtTokens(n: number): string { return n >= 1000 ? (n / 1000).toFixed(1) + 'k' : String(n) }

/** 生命体待投递问候（/api/life/unread 返回的 pending 项）。 */
interface SessionSnapshot {
  session: { messages?: ChatMessageJson[]; archive?: ChatMessageJson[]; cwd?: string; usage?: { input_tokens?: number; output_tokens?: number; total_tokens?: number } }
  events: AgentEvent[]
  event_seq: number
  running: boolean
  phase?: string
  truncated?: boolean
}

interface LifeUnreadItem {
  id: string
  text: string
  trigger: string
  lifeName?: string
  createdAtMs?: number
}

function sanitizeToolName(name: string): string {
  return name.replace(/[^a-zA-Z0-9_.:-]/g, '').slice(0, 80) || 'unknown_tool'
}

function classifyToolError(message: string): string {
  const text = message.toLowerCase()
  if (/permission|denied|allowed area/.test(text)) return 'permission_or_sandbox'
  if (/timeout|timed out/.test(text)) return 'timeout'
  if (/not found|enoent/.test(text)) return 'not_found'
  if (/invalid|schema|argument|parse/.test(text)) return 'invalid_arguments'
  if (/network|connect|dns|http/.test(text)) return 'network_or_upstream'
  return 'execution_error'
}

function summarizeArguments(value: unknown, key = '', depth = 0): unknown {
  if (depth > 4) return '[max_depth]'
  if (value === null) return '[null]'
  if (Array.isArray(value)) return value.slice(0, 12).map(item => summarizeArguments(item, key, depth + 1))
  if (typeof value === 'object') {
    return Object.fromEntries(Object.entries(value as Record<string, unknown>).slice(0, 30).map(([childKey, child]) => [
      childKey.replace(/[^a-zA-Z0-9_.:-]/g, '').slice(0, 80) || 'field',
      summarizeArguments(child, childKey, depth + 1),
    ]))
  }
  if (typeof value === 'boolean') return value
  if (typeof value === 'number') return '[number]'
  if (typeof value !== 'string') return `[${typeof value}]`
  const text = value.trim()
  const lowerKey = key.toLowerCase()
  if (/key|token|secret|password|authorization|credential/.test(lowerKey)) return '[redacted_secret]'
  if (/path|file|dir|cwd|destination|source/.test(lowerKey) || /^(?:\/|[a-z]:\\)/i.test(text)) {
    const extension = text.match(/\.([a-zA-Z0-9]{1,8})$/)?.[1]?.toLowerCase()
    return `[${/^(?:\/|[a-z]:\\)/i.test(text) ? 'absolute' : 'relative'}_path${extension ? ` ext=.${extension}` : ''}]`
  }
  if (/command|cmd|script/.test(lowerKey) || /[\s;&|><]/.test(text)) {
    const tokens = text.split(/\s+/).filter(Boolean)
    const executable = tokens[0]?.split(/[\\/]/).pop()?.replace(/[^a-zA-Z0-9_.+-]/g, '') || 'unknown'
    const flags = tokens.slice(1).filter(token => /^--?[a-zA-Z0-9_-]+$/.test(token)).slice(0, 12)
    return { kind: 'command_shape', executable, flags, token_count: tokens.length, has_shell_operators: /[;&|><]/.test(text) }
  }
  if (/^https?:\/\//i.test(text)) return '[url_redacted]'
  if (/^[a-zA-Z][a-zA-Z0-9_.:-]{0,31}$/.test(text)) return text
  return `[string length=${text.length}]`
}

function sanitizeDiagnosticText(message: string): string {
  return message
    .slice(0, 1200)
    .replace(/\b(?:sk-|Bearer\s+)[a-zA-Z0-9._-]{8,}\b/gi, '[redacted_secret]')
    .replace(/https?:\/\/[^\s"']+/gi, '[redacted_url]')
    .replace(/(?:[a-zA-Z]:\\|\/data\/|\/storage\/|\/sdcard\/|\/home\/)[^\s"']+/g, '[redacted_path]')
    .replace(/[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}/g, '[redacted_email]')
    .replace(/\b[0-9a-f]{24,}\b/gi, '[redacted_identifier]')
    .replace(/(["'])(?:(?!\1).){41,}\1/g, '[redacted_text]')
}

function buildLocalEvidence(items: ToolDiagnosticTrace[]): string {
  return [
    '【程序采集的脱敏证据】',
    '不含用户消息、原始参数值、文件内容、真实路径、URL、密钥或模型隐藏思维。',
    ...items.map(item => `#${item.sequence} ${item.tool} | ${item.status}${item.category ? ` | ${item.category}` : ''}${item.elapsedMs !== undefined ? ` | ${item.elapsedMs}ms` : ''}\n参数结构: ${JSON.stringify(item.argumentShape)}${item.errorSummary ? `\n错误摘要: ${item.errorSummary}` : ''}`),
  ].join('\n')
}

const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i
const ACTIVE_SESSION_KEY = 'coomi.activeSessionId.v1'

function isUuid(value: string): boolean {
  return UUID_PATTERN.test(value)
}

function readActiveSessionId(): string {
  try {
    const saved = localStorage.getItem(ACTIVE_SESSION_KEY) ?? ''
    if (isUuid(saved)) return saved
  } catch {
    // WebView storage can be unavailable during early startup; create a valid fallback.
  }
  return createSessionId()
}

function persistActiveSessionId(id: string) {
  try {
    localStorage.setItem(ACTIVE_SESSION_KEY, id)
  } catch {
    // Keeping the in-memory id is enough for this process lifetime.
  }
}

/** 引擎磁盘上会话文件的原始消息结构（与 coomi-engine 的 ChatMessage 对应）。 */
interface ChatMessageJson {
  reasoning?: string
  reasoning_state?: string
  id?: string
  role: 'system' | 'user' | 'assistant' | 'tool'
  content: string
  tool_calls?: Array<{
    id: string
    name: string
    arguments: unknown
    images?: Array<{ media_type: string; data: string }>
  }>
  tool_call_id?: string
  compaction_summary?: boolean
  internal?: boolean
  life_proactive?: boolean
  images?: Array<{ media_type: string; data: string }>
}

function createSessionId(): string {
  const cryptoApi = globalThis.crypto
  if (typeof cryptoApi?.randomUUID === 'function') return cryptoApi.randomUUID()
  const bytes = new Uint8Array(16)
  cryptoApi.getRandomValues(bytes)
  bytes[6] = (bytes[6] & 0x0f) | 0x40
  bytes[8] = (bytes[8] & 0x3f) | 0x80
  const hex = Array.from(bytes, byte => byte.toString(16).padStart(2, '0')).join('')
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`
}


const useMainSessionStore = defineSessionStore('session')
export type SessionStore = ReturnType<typeof useMainSessionStore>
export const SessionScope: InjectionKey<Ref<SessionStore | null>> = Symbol('session-scope')
export function useSessionStore(): SessionStore {
  const scoped = hasInjectionContext() ? inject(SessionScope, null) : null
  return scoped?.value ?? useMainSessionStore()
}

const auxiliaryStores = new Map<string, ReturnType<typeof defineSessionStore>>()
const auxiliaryTransfers = new Map<string, (paths: string[]) => void>()
export function completePendingFileTransfer(requestId: string, paths: string[]): boolean {
  const complete = auxiliaryTransfers.get(requestId)
  if (!complete) return false
  auxiliaryTransfers.delete(requestId)
  complete(paths)
  return true
}
export function useAuxiliarySessionStore(id: string): SessionStore {
  let definition = auxiliaryStores.get(id)
  if (!definition) {
    definition = defineSessionStore(`auxiliary:${id}`, true)
    auxiliaryStores.set(id, definition)
  }
  return definition()
}
