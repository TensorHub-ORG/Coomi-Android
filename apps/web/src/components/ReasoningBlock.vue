<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue'
import type { ReasoningBlock } from '@/stores/viewModel'
import CoomiIcon from './CoomiIcon.vue'
const props = defineProps<{ block: ReasoningBlock }>()
const body = ref<HTMLElement | null>(null)
const open = ref(false)
const manual = ref(false)
const live = computed(() => props.block.streaming === true)
const chars = computed(() => props.block.content.replace(/\s+/g, '').length)
watch(live, () => { if (!manual.value) open.value = false })
watch(() => props.block.content, async () => { await nextTick(); if (live.value && !open.value && body.value) body.value.scrollTop = body.value.scrollHeight })
function toggle() { manual.value = true; open.value = !open.value }
</script>
<template>
  <div class="reasoning fade-in">
    <button class="toggle" :aria-expanded="open" @click="toggle">
      <CoomiIcon name="sparkle" :size="14" :class="{ live }" />
      <span>{{ live ? '正在思考…' : '思考过程' }}</span>
      <span class="count">{{ chars }} 字{{ block.state === 'interrupted' ? ' · 已中断' : block.state === 'failed' ? ' · 未完成' : '' }}</span>
      <CoomiIcon name="chevronRight" :size="13" class="chev" :class="{ open }" />
    </button>
    <div v-if="open || live" ref="body" class="body" :class="{ preview: live && !open }">{{ block.content }}</div>
  </div>
</template>
<style scoped>
.toggle { display:flex; align-items:center; gap:7px; width:100%; min-height:32px; padding:4px 2px; border:0; background:none; text-align:left; font-size:13px; color:var(--text-3); }
.live { color:var(--blue); animation:coomi-blink 1.4s ease-in-out infinite; }
.count { flex:1; font-size:11.5px; }
.chev { transition:transform .18s; }
.chev.open { transform:rotate(90deg); }
.body { margin:4px 0 0 6px; padding:9px 13px; border-left:2px solid var(--blue-border); border-radius:0 var(--r-sm) var(--r-sm) 0; background:var(--fill); font-size:13px; line-height:1.7; color:var(--term-think); white-space:pre-wrap; overflow-wrap:anywhere; }
.preview { max-height:calc(3.5 * 1.7 * 13px + 18px); overflow:hidden; mask-image:linear-gradient(transparent,#000 28%); }
</style>
