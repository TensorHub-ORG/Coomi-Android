<script setup lang="ts">
/**
 * 计划进度条：由引擎 plan_updated 事件驱动（完整步骤列表 + 真实状态），
 * 与持久化 loop 分离，不再用循环轮数冒充计划步骤。
 * 进度 = 已完成步骤 / 总步骤，状态徽标：完成 / 进行中 / 待办。
 */
import { computed, ref } from 'vue'
import { useSessionStore } from '@/stores/session'
import CoomiIcon from './CoomiIcon.vue'

const session = useSessionStore()

const plan = computed(() => session.plan)
const planSteps = computed(() => plan.value.steps ?? [])
const loopSteps = computed(() => session.loop.currentDescription
  ? [{ step: session.loop.currentDescription, status: 'in_progress' as const }]
  : [])
const steps = computed(() => planSteps.value.length ? planSteps.value : loopSteps.value)
const active = computed(() => (plan.value.active && planSteps.value.length > 0) || session.loop.active)
const expanded = ref(false)

const totalSteps = computed(() => planSteps.value.length ? planSteps.value.length : session.loop.totalSteps)
const completedSteps = computed(() => planSteps.value.length
  ? planSteps.value.filter(s => s.status === 'completed').length
  : session.loop.currentStep)
const pct = computed(() => {
  if (totalSteps.value === 0) return 0
  return Math.round((completedSteps.value / totalSteps.value) * 100)
})
const currentDescription = computed(() => {
  const inProgress = steps.value.find(s => s.status === 'in_progress')
  return inProgress?.step ?? steps.value[steps.value.length - 1]?.step ?? session.loop.currentDescription ?? '执行中'
})
</script>

<template>
  <div v-if="active && totalSteps > 0" class="loop fade-in">
    <button class="head" type="button" @click="expanded = !expanded">
      <CoomiIcon name="subtask" :size="14" class="ic" />
      <span class="tag">计划进度</span>
      <span class="txt">{{ currentDescription }}</span>
      <span class="count">{{ completedSteps }}/{{ totalSteps }}</span>
      <CoomiIcon :name="expanded ? 'chevronUp' : 'chevronDown'" :size="13" class="caret" />
    </button>
    <div class="track"><div class="fill" :style="{ width: pct + '%' }" /></div>
    <Transition name="fold">
      <div v-if="expanded" class="detail">
        <div v-for="(s, i) in steps" :key="i" class="step">
          <span class="dot" :class="s.status" />
          <span class="label">{{ s.step }}</span>
          <em v-if="s.status === 'completed'" class="badge done">已完成</em>
          <em v-else-if="s.status === 'in_progress'" class="badge">进行中</em>
          <em v-else class="badge todo">待办</em>
        </div>
        <p v-if="!steps.length" class="empty">暂无步骤明细</p>
      </div>
    </Transition>
  </div>
</template>

<style scoped>
.loop {
  margin: 2px 12px 4px; padding: 9px 12px 10px;
  border-radius: var(--r-md); background: var(--blue-soft);
}
.head { display: flex; align-items: center; gap: 7px; width: 100%; border: 0; background: none; text-align: left; color: inherit; }
.ic { color: var(--blue); flex-shrink: 0; }
.tag { flex-shrink: 0; font-size: 11.5px; font-weight: 700; color: var(--blue); }
.txt {
  flex: 1; min-width: 0; font-size: 12.5px; color: var(--text-2);
  white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
}
.count { flex-shrink: 0; font-size: 11.5px; color: var(--text-3); font-variant-numeric: tabular-nums; }
.caret { flex-shrink: 0; color: var(--text-3); }
.track { height: 4px; border-radius: 2px; background: var(--bg); overflow: hidden; margin-top: 8px; }
.fill { height: 100%; border-radius: 2px; background: linear-gradient(90deg, var(--blue), #5b87dc); transition: width .35s ease; }
.detail { margin-top: 9px; padding-top: 8px; border-top: 1px solid color-mix(in srgb, var(--blue) 22%, transparent); display: flex; flex-direction: column; gap: 6px; }
.step { display: flex; align-items: center; gap: 7px; font-size: 12.5px; color: var(--text-2); }
.dot { width: 7px; height: 7px; border-radius: 50%; background: var(--text-3); flex-shrink: 0; }
.dot.in_progress { background: var(--blue); animation: pulse 1.1s ease-in-out infinite; }
.dot.completed { background: var(--ok); }
.dot.pending { background: var(--text-3); }
.label { flex: 1; min-width: 0; }
.badge { font-style: normal; font-size: 10.5px; padding: 2px 7px; border-radius: var(--r-pill); background: var(--blue); color: #fff; }
.badge.done { background: var(--ok-soft); color: var(--ok); }
.badge.todo { background: var(--fill); color: var(--text-2); }
.empty { font-size: 12px; color: var(--text-3); }
@keyframes pulse { 50% { opacity: .35; } }
.fold-enter-active, .fold-leave-active { transition: opacity .18s ease, transform .18s ease; }
.fold-enter-from, .fold-leave-to { opacity: 0; transform: translateY(-4px); }
</style>
