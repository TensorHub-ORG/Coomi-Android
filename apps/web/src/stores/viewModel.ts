import type { ToolAccess, UserQuestion } from '@/protocol/events'
import type { ChatAttachment } from '@/utils/attachments'

export type ToolCardStatus = 'starting' | 'running' | 'success' | 'error' | 'awaiting_approval' | 'cache_hit' | 'cancelled'

export interface ToolCard {
  turnId?: string
  kind: 'tool'
  callId: string
  toolName: string
  arguments: Record<string, unknown>
  status: ToolCardStatus
  elapsed?: number
  resultPreview?: string
  isError?: boolean
  access?: ToolAccess
  riskSummary?: string
  expanded?: boolean
  manualOpen?: boolean
  /** 工具产生的图片（data URL），瀑布流渲染用。 */
  images?: string[]
  /** show_image 历史恢复但图片数据不可用（如已被上下文压缩清理）。 */
  imageMissing?: boolean
}

export interface AssistantMessage { kind: 'assistant'; id: string; mid: string; content: string; streaming: boolean; life?: boolean }
export interface UserMessage {
  kind: 'user'; id: string; mid: string; content: string
  /** 文件附件独立于消息正文渲染。 */
  attachments?: ChatAttachment[]
  /** 仅用于本次渲染的发送液滴状态，不写入引擎协议。 */
  morphing?: boolean
  morphArrived?: boolean
}
export interface ReasoningBlock { kind: 'reasoning'; id: string; content: string; expanded: boolean; streaming?: boolean; state?: string; mid?: string }

export interface QuestionCard {
  kind: 'question'; callId: string; questions: UserQuestion[]
  answered: boolean; answers?: Record<string, string>
}

export interface NoticeItem {
  kind: 'notice'
  id: string
  tone: 'info' | 'warn' | 'error' | 'success'
  text: string
  detail?: string
  feedbackEligible?: boolean
  analysisStatus?: 'consent' | 'analyzing' | 'ready' | 'uploading' | 'complete' | 'failed'
  analysisTrace?: ToolDiagnosticTrace[]
  failureCount?: number
}

export interface ToolDiagnosticTrace {
  callId?: string
  sequence: number
  tool: string
  argumentShape: unknown
  status: 'running' | 'error' | 'success'
  category?: string
  errorSummary?: string
  elapsedMs?: number
}

export type Timelineitem = (UserMessage | AssistantMessage | ReasoningBlock | ToolCard | QuestionCard | NoticeItem) & { turnId?: string }

export type RunState = 'idle' | 'syncing' | 'thinking' | 'executing' | 'awaiting_approval' | 'awaiting_question'

export interface LoopProgress {
  active: boolean; currentStep: number; totalSteps: number
  status: string; currentDescription?: string
}

/** 引擎 plan_updated 的完整步骤列表；与持久化 loop 分开。 */
export interface PlanStep { step: string; status: 'pending' | 'in_progress' | 'completed' }
export interface PlanProgress { active: boolean; steps: PlanStep[]; explanation?: string }
