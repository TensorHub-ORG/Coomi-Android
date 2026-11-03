<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import PageHead from '@/components/PageHead.vue'
import { splitUpdateNotes } from '@/utils/updateNotes'
import { goBack } from '@/bridge/navigation'

/** 更新通道：正式 / 测试，元数据放在 GitHub 仓库，经 gh-proxy.com 加速。 */
const GITHUB_UPDATES = 'https://gh-proxy.com/https://raw.githubusercontent.com/TensorHub-ORG/Coomi-Android/main/deploy/updates'
const CHANNELS = {
  stable: { label: '正式更新', dir: `${GITHUB_UPDATES}/android` },
  test: { label: '测试更新', dir: `${GITHUB_UPDATES}/android-test` },
} as const

type Channel = keyof typeof CHANNELS
interface UpdateInfo {
  versionCode: number
  version: string
  notes: string
  date: string
  channel: Channel
  apkUrl?: string
}

const router = useRouter()
const channel = ref<Channel>('stable')
const loading = ref(true)
const installing = ref(false)
const error = ref('')
const info = ref<UpdateInfo | null>(null)
let refreshGeneration = 0

const canInstall = computed(() => Boolean(window.CoomiAndroid?.installApk))

// 更新说明支持多版本分段：以「vX.Y.Z 更新说明【…】」标题行为分段边界。
// 首段为当前版本说明常驻显示，历史段落默认收起，点「展开全部更新记录」逐段展开。
const noteSections = computed(() => splitUpdateNotes(info.value?.notes ?? ''))
const latestNotes = computed(() => noteSections.value[0] ?? (info.value?.notes ?? '').trim())
const historyNotes = computed(() => noteSections.value.slice(1))
const historyExpanded = ref(false)

// 批次七 #9：与当前版本比对——更新页明确给出「已是最新 / 有新版本」状态，
// 已是最新时隐藏下载按钮（旧版会重复下载同版本包）。
const hasUpdate = computed(() => Boolean(info.value && info.value.versionCode > currentCode.value))
const versionStatusText = computed(() => {
  if (!info.value) return ''
  if (hasUpdate.value) return `有新版本 v${info.value.version}`
  if (info.value.versionCode && info.value.versionCode === currentCode.value) return '已是最新版本'
  return '当前版本较新（或版本号无法比较）'
})

async function refresh() {
  const generation = ++refreshGeneration
  const selectedChannel = channel.value
  loading.value = true
  error.value = ''
  info.value = null
  historyExpanded.value = false
  const dir = CHANNELS[channel.value].dir
  try {
    const response = await fetch(`${dir}/latest.json`, { cache: 'no-store' })
    if (!response.ok) throw new Error(`更新源返回 HTTP ${response.status}`)
    const data = await response.json()
    const file: string | undefined = data.file
    const directUrl: string | undefined = typeof data.url === 'string' && data.url ? data.url : undefined
    if (generation !== refreshGeneration) return
    info.value = {
      versionCode: Number(data.versionCode) || 0,
      version: String(data.version ?? ''),
      notes: String(data.notes ?? ''),
      date: String(data.date ?? ''),
      channel: selectedChannel,
      apkUrl: directUrl ?? (file ? `${dir}/${file}` : undefined),
    }
  } catch (e) {
    if (generation !== refreshGeneration) return
    error.value = e instanceof Error ? e.message : String(e)
  } finally {
    if (generation !== refreshGeneration) return
    loading.value = false
    void refreshDots()
  }
}

function switchChannel(next: Channel) {
  if (channel.value === next) return
  channel.value = next
  void refresh()
}

function install() {
  if (!info.value?.apkUrl || !info.value.version || !canInstall.value) return
  installing.value = true
  window.CoomiAndroid!.installApk!(info.value.apkUrl, info.value.version)
  setTimeout(() => { installing.value = false }, 1500)
}

const currentCode = ref(0)
const currentVersionName = ref('')
// 批次七红点策略：两通道各自有可更新版本时，在标签文字右侧亮小红点；
// 测试通道的红点提醒可由用户开关（默认关）。
const channelHasUpdate = ref<Record<Channel, boolean>>({ stable: false, test: false })
const remindersEnabled = ref(window.CoomiAndroid?.getUpdateRemindersEnabled?.() ?? localStorage.getItem('coomi.updateReminders') !== '0')
function setReminders(event: Event) {
  const enabled = (event.target as HTMLInputElement).checked
  remindersEnabled.value = enabled
  localStorage.setItem('coomi.updateReminders', enabled ? '1' : '0')
  window.CoomiAndroid?.setUpdateRemindersEnabled?.(enabled)
}
const testDotEnabled = ref(false)
const canToggleTestDot = computed(() => Boolean(window.CoomiAndroid?.setTestUpdateDotEnabled))

async function refreshDots() {
  const code = currentCode.value
  if (!code) return
  for (const key of ['stable', 'test'] as Channel[]) {
    try {
      const response = await fetch(`${CHANNELS[key].dir}/latest.json`, { cache: 'no-store' })
      if (!response.ok) continue
      const data = await response.json()
      channelHasUpdate.value[key] = (Number(data.versionCode) || 0) > code
    } catch { /* 单通道失败不影响另一个 */ }
  }
}

function loadTestDot() {
  testDotEnabled.value = window.CoomiAndroid?.getTestUpdateDotEnabled?.() ?? true
}

function setTestDot(event: Event) {
  const enabled = (event.target as HTMLInputElement).checked
  testDotEnabled.value = enabled
  window.CoomiAndroid?.setTestUpdateDotEnabled?.(enabled)
}

onMounted(() => {
  currentCode.value = window.CoomiAndroid?.getAppVersionCode?.() ?? 0
  currentVersionName.value = window.CoomiAndroid?.getAppVersionName?.() ?? ''
  if (currentVersionName.value.includes('-test')) channel.value = 'test'
  loadTestDot()
  void refreshDots()
  void refresh()
})
</script>

<template>
  <div class="page">
    <PageHead title="检查更新" @back="goBack(router, 'dashboard')" />

    <main class="body">
      <div class="channel-tabs">
        <button
          v-for="(item, key) in CHANNELS"
          :key="key"
          :class="{ selected: channel === key }"
          @click="switchChannel(key)"
        >
          <span class="tab-label">{{ item.label }}<i v-if="channelHasUpdate[key]" class="tab-dot" /></span>
        </button>
      </div>

      <label class="test-dot-toggle"><span>更新提醒</span><input type="checkbox" :checked="remindersEnabled" @change="setReminders" /></label>
      <label v-if="canToggleTestDot" class="test-dot-toggle">
        <span>测试通道有更新时也亮红点提醒</span>
        <input type="checkbox" :checked="testDotEnabled" @change="setTestDot" />
      </label>

      <p v-if="channel === 'test'" class="notice">测试版包含实验性改动，可能存在不稳定情况。请按需安装。</p>
      <p v-if="error" class="notice error">{{ error }}</p>
      <p v-if="loading" class="notice">正在获取更新信息…</p>

      <template v-if="info">
        <section class="group">
          <div class="line"><span>更新通道</span><strong>{{ CHANNELS[info.channel].label }}</strong></div>
          <div class="line">
            <span>版本状态</span>
            <strong :class="{ fresh: hasUpdate }">{{ versionStatusText }}</strong>
          </div>
          <div class="line"><span>最新版本</span><strong>v{{ info.version }}（build {{ info.versionCode }}）</strong></div>
          <div v-if="info.date" class="line"><span>发布日期</span><strong>{{ info.date }}</strong></div>
          <div class="line"><span>本地版本</span><strong>{{ currentVersionName || '—' }}（build {{ currentCode || '—' }}）</strong></div>
          <div v-if="latestNotes" class="notes">{{ latestNotes }}</div>
          <template v-if="historyNotes.length">
            <div v-for="(section, i) in historyNotes" :key="i" v-show="historyExpanded" class="notes notes-history">{{ section }}</div>
            <button class="history-toggle" @click="historyExpanded = !historyExpanded">
              {{ historyExpanded ? '收起更新记录' : `展开全部更新记录（含 ${historyNotes.length} 条历史版本）` }}
            </button>
          </template>
        </section>

        <button v-if="hasUpdate" class="primary" :disabled="installing || !canInstall" @click="install">
          {{ installing ? '正在下载安装…' : info.channel === 'test' ? '下载并安装测试版' : '下载并安装' }}
        </button>
        <p v-else class="notice">当前已是该通道的最新版本，无需更新。</p>
      </template>
    </main>
  </div>
</template>

<style scoped>
.page { display: flex; flex-direction: column; height: 100%; background: var(--page); }
.body { flex: 1; overflow: auto; padding: 14px 12px calc(var(--safe-bottom) + 24px); }
.channel-tabs { display: flex; gap: 8px; margin-bottom: 22px; }
.channel-tabs button {
  position: relative; flex: 1; height: 38px; border-radius: 8px;
  background: var(--fill); color: var(--text-2); font-size: 13px;
}
.tab-label { position: relative; display: inline-block; }
.tab-dot {
  position: absolute; top: -4px; right: -11px;
  width: 7px; height: 7px; border-radius: 50%;
  background: var(--danger, #e5534b);
  box-shadow: 0 0 0 2px var(--page, #fff);
}
.test-dot-toggle {
  display: flex; align-items: center; justify-content: space-between; gap: 10px;
  margin: 0 0 14px; padding: 10px 12px;
  border-radius: 8px; background: var(--fill);
  color: var(--text-2); font-size: 12.5px;
}
.test-dot-toggle input { width: 18px; height: 18px; accent-color: var(--blue); }
.channel-tabs button.selected { background: var(--blue); color: #fff; }
.notice { margin: 0 0 10px; padding: 8px 12px; border-radius: 8px; background: var(--blue-soft); color: var(--blue); font-size: 12.5px; }
.fresh { color: var(--focus, #b4690e); }
.notice.error { background: color-mix(in srgb, var(--orange) 16%, var(--bg)); color: var(--orange); }
.group { margin-bottom: 12px; overflow: hidden; border-radius: var(--r-card); background: var(--bg); box-shadow: var(--shadow-1); }
.line { display: flex; align-items: center; justify-content: space-between; gap: 10px; padding: 12px 13px; border-bottom: 1px solid var(--border); font-size: 13px; }
.line:last-of-type { border-bottom: 0; }
.line span { color: var(--text-3); }
.line strong { color: var(--text); }
.notes { padding: 12px 13px; font-size: 12.5px; line-height: 1.6; color: var(--text-2); white-space: pre-wrap; word-break: break-word; }
.notes-history { border-top: 1px dashed var(--border); color: var(--text-3); }
.history-toggle {
  width: 100%; padding: 10px 13px; border-top: 1px solid var(--border);
  background: var(--fill); color: var(--blue); font-size: 12.5px;
}
.primary, .secondary { display: inline-flex; align-items: center; justify-content: center; width: 100%; height: 44px; border-radius: 10px; background: var(--blue); color: #fff; font-size: 14px; }
.primary:disabled { opacity: 0.55; }
</style>
