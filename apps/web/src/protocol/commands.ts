import type { AgentEvent } from './events'

export type PermissionMode = 'ask' | 'auto' | 'full' | 'minimal'
export type ApprovalDecision = 'allow' | 'deny' | 'always'
export type ReasoningEffort = 'off' | 'auto' | 'low' | 'medium' | 'high' | 'xhigh'
export type SessionMode = 'agent' | 'life'

export interface SendMessageCommand { command: 'send_message'; text: string }
export interface CancelCommand { command: 'cancel' }
export interface JumpInCommand { command: 'jump_in'; text: string }
export interface ApproveToolCommand { command: 'approve_tool'; call_id: string; decision: ApprovalDecision }
export interface AnswerQuestionCommand { command: 'answer_question'; call_id: string; answers: Record<string, string> }
export interface SetPermissionModeCommand { command: 'set_permission_mode'; mode: PermissionMode }
export interface SetSessionModeCommand { command: 'set_session_mode'; mode: SessionMode }
export interface EnterPlanModeCommand { command: 'enter_plan_mode' }
export interface ExitPlanModeCommand { command: 'exit_plan_mode' }
export interface SelectModelCommand { command: 'select_model'; provider_id: string; model: string }
export interface FileTransferResultCommand { command: 'file_transfer_result'; request_id: string; paths: string[] }
export interface SendGuideCommand { command: 'send_guide'; key: string }
export interface RetryTurnCommand { command: 'retry_turn' }
export interface RegenerateResponseCommand { command: 'regenerate_response'; msg_id: string }
export interface EditTurnCommand { command: 'edit_turn'; msg_id: string; text: string }
export interface UndoTurnCommand { command: 'undo_turn'; msg_id: string }
export interface SetReasoningEffortCommand { command: 'set_reasoning_effort'; effort: ReasoningEffort }
export interface SetProductionModeCommand { command: 'set_production_mode'; mode?: 'normal' | 'overload' | 'berserk'; enabled?: boolean }
export interface SetMaxToolRoundsCommand { command: 'set_max_tool_rounds'; rounds: number }
export interface AckEventCommand { command: 'ack_event'; event_seq: number }
/** 数字生命体：把队列里唯一的 pending 问候投递到当前会话（气泡）。 */
export interface DeliverLifeCommand { command: 'deliver_life' }

export type AgentCommand =
  | SendMessageCommand | CancelCommand | JumpInCommand | ApproveToolCommand
  | AnswerQuestionCommand | SetPermissionModeCommand | SetSessionModeCommand | EnterPlanModeCommand
  | ExitPlanModeCommand | SelectModelCommand | FileTransferResultCommand
  | SendGuideCommand | RetryTurnCommand | RegenerateResponseCommand | EditTurnCommand | UndoTurnCommand
  | SetReasoningEffortCommand | SetProductionModeCommand | SetMaxToolRoundsCommand | AckEventCommand
  | DeliverLifeCommand

export const PROTOCOL_VERSION = 1
export type EnvelopeType = 'event' | 'command' | 'ack' | 'error'

export interface Envelope<P = unknown> {
  v: number; type: EnvelopeType; id?: string; ts: number; payload: P
}

export type EventEnvelope = Envelope<AgentEvent> & { type: 'event' }
export type CommandEnvelope = Envelope<AgentCommand> & { type: 'command' }
export type AckEnvelope = Envelope<{ ok: boolean }> & { type: 'ack' }
export type ErrorEnvelope = Envelope<{ message: string; code?: string }> & { type: 'error' }
export type InboundEnvelope = EventEnvelope | AckEnvelope | ErrorEnvelope
