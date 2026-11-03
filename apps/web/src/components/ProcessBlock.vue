<script setup lang="ts">
/**
 * 过程块：把一轮里多段思考 + 多次工具调用折叠到同一个标题下。
 *
 * 标题给出「已思考 N 段 · 使用工具 M 次」，运行中显示进行中数量，
 * 结束后给出成功/失败统计；点开才铺开细节，内部仍复用 TimelineEntry。
 * 展开/收回用 grid-template-rows 0fr ↔ 1fr，不需要量高度。
 */
import { computed, ref, watch } from 'vue'
import type { TimelineBlockItem } from '@/utils/chatTimeline'
import TimelineEntry from './TimelineEntry.vue'
import CoomiIcon from './CoomiIcon.vue'

const props = defineProps<{ block: Extract<TimelineBlockItem, { t: 'process' }> }>()

const open = ref(props.block.running)
const mounted = ref(props.block.running)
const userToggled = ref(false)

watch(() => props.block.running, (running, previous) => {
  if (running) {
    // 新的一轮过程：保持展开，能看到实时进度。
    open.value = true
    mounted.value = true
    userToggled.value = false
    return
  }
  // 一轮结束：自动折回一行摘要；用户手动展开过就不再强制合上。
  if (previous && !userToggled.value) open.value = false
}, { immediate: true })

const reasoningStages = computed(() => props.block.blocks.filter(b => b.t === 'one' && b.item.kind === 'reasoning').length)
const cards = computed(() => props.block.blocks.flatMap(b => (b.t === 'tools' ? b.cards : [])))
const running = computed(() => cards.value.filter(card => card.status === 'running' || card.status === 'starting' || card.status === 'awaiting_approval').length)
const succeeded = computed(() => cards.value.filter(card => card.status === 'success' || card.status === 'cache_hit').length)
const failed = computed(() => cards.value.filter(card => card.status === 'error' || card.status === 'cancelled').length)

const summary = computed(() => {
  const parts: string[] = [reasoningStages.value > 0 ? `已思考 ${reasoningStages.value} 段` : '已思考']
  if (cards.value.length > 0) parts.push(`使用工具 ${cards.value.length} 次`)
  if (props.block.running && running.value > 0) parts.push(`进行中 ${running.value}`)
  if (!props.block.running && succeeded.value > 0) parts.push(`成功 ${succeeded.value}`)
  if (!props.block.running && failed.value > 0) parts.push(`失败 ${failed.value}`)
  return parts.join(' · ')
})

function toggle() {
  // 运行中保持展开：正在思考/调工具的时候收起来会看不到进度。
  if (props.block.running && open.value) return
  open.value = !open.value
  userToggled.value = true
  if (open.value) mounted.value = true
}
</script>

<template>
  <section class="process-block">
    <button class="process-toggle" :aria-expanded="open" @click="toggle">
      <CoomiIcon name="sparkle" :size="14" :class="{ pulse: block.running }" />
      <span class="summary">{{ summary }}</span>
      <CoomiIcon name="chevronRight" :size="13" class="chev" :class="{ open }" />
    </button>
    <div class="process-wrap" :class="{ open, live: block.running }">
      <div class="process-inner">
        <div v-if="mounted" class="process-content"><TimelineEntry v-for="entry in block.blocks" :key="entry.key" :block="entry" /></div>
      </div>
    </div>
  </section>
</template>

<style scoped>
.process-block { min-width:0; width:100%; border:1px solid var(--border); border-radius:var(--r-md); background:var(--bg); overflow:hidden; }
.process-toggle { display:flex; align-items:center; gap:7px; width:100%; padding:10px; background:var(--fill); color:var(--text-2); text-align:left; font-size:12px; }
.process-toggle .summary { flex:1; min-width:0; font-weight:600; }
.process-toggle .pulse { color:var(--blue); animation:coomi-blink 1.4s ease-in-out infinite; }
.process-toggle .chev { flex-shrink:0; color:var(--text-3); transition:transform .18s cubic-bezier(.22,.68,.19,1); }
.process-toggle .chev.open { transform:rotate(90deg); }
.process-wrap { display:grid; grid-template-rows:0fr; }
.process-wrap.open { grid-template-rows:1fr; }
.process-wrap:not(.live) { transition:grid-template-rows .26s cubic-bezier(.22,.68,.19,1); }
.process-wrap.live { transition:none; }
.process-inner { min-height:0; overflow:hidden; }
.process-content { display:flex; flex-direction:column; gap:10px; padding:10px; min-width:0; }
</style>
