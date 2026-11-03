export type ToolAccess = 'read_only' | 'write' | 'destructive'

export interface UsageInfo {
  input_tokens: number; output_tokens: number; total_tokens: number
  cached_input_tokens?: number
  cache_hit_rate?: number | null
  cache_data_available?: boolean
  turn_cache_hit_rate?: number | null
  turn_cache_data_available?: boolean
  /** 本轮输出 token / 已耗时，用于状态栏实时速率（token/s）与延迟展示。 */
  turn_output_tokens?: number
  turn_elapsed_ms?: number
  turn_rate_tps?: number
  context_ratio?: number
  context_used_tokens?: number
  context_window_tokens?: number
}
export interface ReasoningEffortStats {
  turns: number
  cache_hit_rate: number | null
  average_duration_ms: number | null
  average_total_tokens: number | null
  cache_available: boolean
}

export interface TextChunkEvent { event_type: 'text_chunk'; content: string }
export interface ReasoningEndEvent { event_type: 'reasoning_end' }
export interface ReasoningChunkEvent { event_type: 'reasoning_chunk'; content: string }
export interface ToolStartEvent { event_type: 'tool_start'; call_id: string; tool_name: string; arguments: Record<string, unknown> }
export interface ToolRunningEvent { event_type: 'tool_running'; call_id: string; tool_name: string }
export interface ToolDoneEvent { event_type: 'tool_done'; call_id: string; tool_name: string; elapsed: number; result_preview: string; is_error: boolean; images?: string[] }
export interface ToolCacheHitEvent { event_type: 'tool_cache_hit'; call_id: string; tool_name: string }
export interface UsageUpdateEvent {
  event_type: 'usage_update'
  usage: UsageInfo
  reasoning_efforts?: Partial<Record<'auto' | 'low' | 'medium' | 'high' | 'xhigh', ReasoningEffortStats>>
  context_categories?: Partial<Record<'system_tools' | 'messages' | 'skills' | 'mcp_tools' | 'system_prompt' | 'other', number>>
}
export interface ConnectionRetryEvent { event_type: 'connection_retry'; attempt: number; max_attempts: number; delay?: number; delay_ms?: number; message: string }
export interface StreamResetEvent { event_type: 'stream_reset' }
export interface CompressionEvent { event_type: 'compression'; before: number; after: number; summary_id?: string; summary?: string }
export interface CompressionStartedEvent { event_type: 'compression_started'; automatic: boolean }
export interface AgentErrorEvent { event_type: 'agent_error'; message: string; is_fatal: boolean; captcha_required?: boolean }
export interface ConfigurationRequiredEvent { event_type: 'configuration_required'; message: string; route: '/providers' }
export interface RetryConfirmationEvent { event_type: 'retry_confirmation'; message: string }
export interface AgentCancelledEvent { event_type: 'agent_cancelled' }
export interface BgTaskDetachedEvent { event_type: 'bg_task_detached'; task_id: string; tool_name: string }
export interface BgTaskCompletedEvent { event_type: 'bg_task_completed'; task_id: string; tool_name: string; is_error: boolean }
export interface LoopStepStartEvent { event_type: 'loop_step_start'; step_index: number; step_description: string; total_steps: number }
export interface LoopStepDoneEvent { event_type: 'loop_step_done'; step_index: number; success: boolean }
export interface LoopProgressEvent { event_type: 'loop_progress'; current_step: number; total_steps: number; status: string }
export interface LoopIssueCreatedEvent { event_type: 'loop_issue_created'; step_index: number; step_description: string }
/** 引擎 PlanUpdated 事件：完整计划步骤，不再用 Loop 轮数冒充。 */
export interface PlanStep { step: string; status: 'pending' | 'in_progress' | 'completed' }
export interface PlanUpdatedEvent { event_type: 'plan_updated'; steps: PlanStep[]; explanation?: string }
export interface ToolApprovalRequestEvent { event_type: 'tool_approval_request'; call_id: string; tool_name: string; arguments: Record<string, unknown>; access: ToolAccess; risk_summary?: string; }
export interface UserQuestionOption { label: string; description: string }
export interface UserQuestion { id: string; header: string; question: string; options: UserQuestionOption[] }
export interface UserQuestionRequestEvent { event_type: 'user_question_request'; call_id: string; questions: UserQuestion[] }
export interface FileTransferRequestEvent { event_type: 'file_transfer_request'; request_id: string; operation: 'import' | 'export'; path?: string; suggested_name?: string; multiple: boolean }
export interface TurnEndEvent { event_type: 'turn_end' }
export interface TurnTruncatedEvent { event_type: 'turn_truncated' }
/** 重连补发：会话是否正在后台执行（切走会话后任务继续跑）。 */
export interface SessionStateEvent { event_type: 'session_state'; running: boolean }
export interface SessionLoadedEvent {
  event_type: 'session_loaded'
  session_id: string
  cwd: string
  usage: { input_tokens: number; output_tokens: number; total_tokens: number }
}
/** 数字生命体主动消息投递完成（气泡/开场问候）。 */
export interface LifeDeliveredEvent {
  event_type: 'life_delivered'
  message_id: string
  trigger: string
  text: string
}

export interface UserMessageEvent { event_type: 'user_message' | 'assistant_message'; message_id: string; content: string; reasoning?: string; reasoning_state?: string }

export type AgentEvent = (
  | UserMessageEvent
  | TextChunkEvent | ReasoningChunkEvent | ReasoningEndEvent | ToolStartEvent | ToolRunningEvent
  | ToolDoneEvent | ToolCacheHitEvent | UsageUpdateEvent | ConnectionRetryEvent | StreamResetEvent
  | CompressionEvent | CompressionStartedEvent | AgentErrorEvent | ConfigurationRequiredEvent | AgentCancelledEvent | BgTaskDetachedEvent
  | BgTaskCompletedEvent | LoopStepStartEvent | LoopStepDoneEvent | LoopProgressEvent
  | LoopIssueCreatedEvent | PlanUpdatedEvent | RetryConfirmationEvent | ToolApprovalRequestEvent | UserQuestionRequestEvent
  | FileTransferRequestEvent
  | TurnEndEvent
  | TurnTruncatedEvent
  | SessionStateEvent
  | SessionLoadedEvent
  | LifeDeliveredEvent
) & { event_seq?: number }

export type AgentEventType = AgentEvent['event_type']
