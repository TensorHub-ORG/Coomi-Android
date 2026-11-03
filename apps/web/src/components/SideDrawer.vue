<script setup lang="ts">
/**
 * 左侧会话抽屉。
 * DeepSeek 的抽屉是「推开主内容」而不是盖住，主体的位移由 ChatView 负责，
 * 这里只管面板自己的滑入、搜索、分组列表和行内操作。
 */
import { computed, nextTick, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { useSessionStore } from '@/stores/session'
import { useConfigStore } from '@/stores/config'
import { formatSessionTime, useSessionsStore, type SessionMeta } from '@/stores/sessions'
import { GLOBAL_SESSION_ID } from '@/bridge/life'
import CoomiIcon from './CoomiIcon.vue'
import AuxiliarySessionList from './AuxiliarySessionList.vue'

const props = defineProps<{ open: boolean }>()
const emit = defineEmits<{ close: [] }>()

const router = useRouter()
const session = useSessionStore()
const sessions = useSessionsStore()
const config = useConfigStore()

const menuFor = ref<SessionMeta | null>(null)
const renamingId = ref('')
const renameText = ref('')

const isEmpty = computed(() => sessions.groups.length === 0)

/** 全局常驻会话：标题跟随生命体开关/名称，未读显示数字徽标，永不消失。 */
const globalMeta = computed(() =>
  sessions.ensure(GLOBAL_SESSION_ID, config.digitalLifeEnabled ? '数字生命体' : '全局对话'),
)
const globalTitle = computed(() => {
  const metaTitle = globalMeta.value.title
  if (session.lifeUnreadName) return session.lifeUnreadName
  if (config.digitalLifeEnabled) {
    return metaTitle && metaTitle !== '新对话' ? metaTitle : '数字生命体'
  }
  return metaTitle && metaTitle !== '新对话' ? metaTitle : '全局对话'
})
const globalRunning = computed(() => sessions.isRunning(GLOBAL_SESSION_ID))
const isGlobalMeta = (m: SessionMeta | null) => m?.id === GLOBAL_SESSION_ID

// 抽屉一关就把临时态清掉；打开时立即刷新一次各会话的「后台运行中」
// 状态（常驻轮询由 ChatView 全局负责，这里只保证打开瞬间是最新的）。
watch(() => props.open, v => {
  if (!v) {
    menuFor.value = null; renamingId.value = ''
    return
  }
  void sessions.syncFromEngine()
  sessions.refreshRunning()
})

function pick(id: string) {
  if (renamingId.value) return
  const parentId = sessions.find(id)?.parentSessionId
  if (parentId) window.dispatchEvent(new CustomEvent('coomi:open-auxiliary', { detail: { parentId, sessionId: id } }))
  else void session.openSession(id)
  emit('close')
}

function startNew() {
  session.newSession()
  emit('close')
}

function closeMenu() { menuFor.value = null }

/** WebView 里 window.prompt 默认被吞掉，所以重命名走行内输入框。 */
async function beginRename() {
  const m = menuFor.value
  if (!m) return
  renamingId.value = m.id
  renameText.value = m.title
  menuFor.value = null
  await nextTick()
  const el = document.querySelector<HTMLInputElement>('.drawer-root .rename')
  el?.focus()
  el?.select()
}

async function commitRename() {
  if (!renamingId.value) return
  const id = renamingId.value
  if (await sessions.rename(id, renameText.value)) renamingId.value = ''
}

async function doPin() {
  if (!menuFor.value) return
  if (await sessions.togglePin(menuFor.value.id)) closeMenu()
}

async function doDelete() {
  if (!menuFor.value) return
  if (menuFor.value.parentSessionId) await sessions.removeAuxiliary(menuFor.value.id)
  else session.deleteSession(menuFor.value.id)
  closeMenu()
}

function go(path: string) {
  // 先收起抽屉再跳路由：App.vue 是 out-in 过渡，如果先 push 再 close，
  // 抽屉退场动画和页面切换动画同时跑，Settings 页面会先被旧页盖住一瞬，
  // 看起来就像「点设置没反应」。
  emit('close')
  nextTick(() => router.push(path))
}
/** 会话累计 token 显示：1.2k / 3.4M。 */
function formatTokens(n: number): string {
  if (n >= 1_000_000) return (n / 1_000_000).toFixed(1) + 'M'
  if (n >= 1_000) return (n / 1_000).toFixed(1) + 'k'
  return String(n)
}
function openDashboard() {
  emit('close')
  if (window.CoomiAndroid?.openDashboard) window.CoomiAndroid.openDashboard()
  else window.location.href = 'coomi://dashboard'
}
</script>

<template>
  <div class="drawer-root" :class="{ open }">
    <div class="scrim" @click="emit('close')" />

    <aside class="panel" role="dialog" aria-label="会话历史">
      <header class="dhead">
        <div class="sfield">
          <CoomiIcon name="search" :size="17" />
          <input v-model="sessions.query" type="text" placeholder="搜索历史会话" enterkeyhint="search" />
          <button v-if="sessions.query" class="clr" aria-label="清空" @click="sessions.query = ''">
            <CoomiIcon name="close" :size="12" />
          </button>
        </div>
      </header>

      <button class="newrow" @click="startNew">
        <span class="nicon"><CoomiIcon name="pencil" :size="17" /></span>
        <span>开启新对话</span>
      </button>
      <div class="entry-grid" aria-label="工具入口">
        <button class="entry" @click="go('/studio')"><CoomiIcon name="sparkle" :size="17" /><span>AI 工作室·预览</span></button>
        <button class="entry" @click="go('/collab')"><CoomiIcon name="team" :size="17" /><span>协同工作台·预览</span></button>
        <button class="entry" @click="go('/files')"><CoomiIcon name="folder" :size="17" /><span>文件</span></button>
        <button class="entry" @click="go('/im')"><CoomiIcon name="chat" :size="17" /><span>群聊</span></button>
        <button class="entry" @click="go('/catalog')"><CoomiIcon name="cube" :size="17" /><span>拓展</span></button>
        <button class="entry" @click="go('/browser')"><CoomiIcon name="globe" :size="17" /><span>浏览器</span></button>
      </div>
      <button class="taskrow" @click="go('/tasks')">
        <CoomiIcon name="subtask" :size="17" />
        <span>任务中心</span>
        <span v-if="sessions.runningIds.size" class="task-count">{{ sessions.runningIds.size }}</span>
      </button>

      <div class="list">
        <div
          class="row global-row"
          :class="{ cur: session.sessionId === GLOBAL_SESSION_ID }"
          @click="pick(GLOBAL_SESSION_ID)"
        >
          <span class="gicon"><CoomiIcon name="lifeRings" :size="17" /></span>
          <div class="rmain">
            <p class="rtitle">{{ globalTitle }}</p>
            <p class="rmeta">
              <span>{{ formatSessionTime(globalMeta.updatedAt) }}</span>
              <span v-if="session.lifeUnread.length" class="global-badge" aria-label="生命体未读消息">{{ session.lifeUnread.length }}</span>
              <span v-if="globalRunning" class="rspin" aria-label="后台运行中" />
            </p>
          </div>
          <button class="rmore" aria-label="更多" @click.stop="menuFor = globalMeta">
            <CoomiIcon name="more" :size="17" />
          </button>
        </div>
        <AuxiliarySessionList :parent-id="GLOBAL_SESSION_ID" @select="pick" @more="menuFor = $event" />
        <p v-if="isEmpty" class="empty">
          还没有历史会话。<br />随便说点什么，标题会用你的第一句话。
        </p>
        <template v-for="g in sessions.groups" :key="g.label">
          <p class="sec-label">{{ g.label }}</p>
          <div v-for="m in g.items" :key="m.id" class="session-entry">
          <div
            class="row"
            :class="{ cur: m.id === session.sessionId }"
            @click="pick(m.id)"
          >
            <div class="rmain">
              <input
                v-if="renamingId === m.id"
                v-model="renameText"
                class="rename"
                @click.stop
                @keyup.enter="commitRename"
                @blur="commitRename"
              />
              <p v-else class="rtitle">{{ m.title }}</p>
              <p class="rmeta">
                <CoomiIcon v-if="m.pinned" name="pin" :size="11" />
                <span v-if="m.parentSessionId" class="aux-badge">辅助</span>
                <span>{{ formatSessionTime(m.updatedAt) }}</span>
                <template v-if="m.turns">
                  <span>·</span><span>{{ m.turns }} 轮</span>
                </template>
                <template v-if="m.totalTokens">
                  <span>·</span><span class="token-count">{{ formatTokens(m.totalTokens) }} tokens</span>
                </template>
                <span v-if="sessions.isRunning(m.id)" class="rspin" aria-label="后台运行中" />
              </p>
            </div>
            <button class="rmore" aria-label="更多" @click.stop="menuFor = m">
              <CoomiIcon name="more" :size="17" />
            </button>
          </div>
          <AuxiliarySessionList :parent-id="m.id" @select="pick" @more="menuFor = $event" />
          </div>
        </template>
      </div>

      <footer class="dfoot">
        <button class="frow console" @click="openDashboard">
          <CoomiIcon name="terminal" :size="20" />
          <span class="fname">返回控制台</span>
        </button>
        <button class="fsq" aria-label="设置" @click="go('/settings')">
          <CoomiIcon name="settings" :size="20" />
        </button>
      </footer>
    </aside>

    <div v-if="menuFor" class="sheet-wrap" @click.self="closeMenu">
      <div class="sheet">
        <p class="sheet-title">{{ menuFor.title }}</p>
          <button class="sheet-item" @click="beginRename">
            <CoomiIcon name="pencil" :size="18" /><span>重命名</span>
          </button>
          <template v-if="!isGlobalMeta(menuFor)">
            <button class="sheet-item" @click="doPin">
              <CoomiIcon name="pin" :size="18" /><span>{{ menuFor.pinned ? '取消置顶' : '置顶' }}</span>
            </button>
            <button class="sheet-item danger" @click="doDelete">
              <CoomiIcon name="trash" :size="18" /><span>删除会话</span>
            </button>
          </template>
          <p v-else class="sheet-hint">全局常驻会话：永远置顶，不可删除</p>
          <button class="sheet-cancel" @click="closeMenu">取消</button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.drawer-root { position: fixed; inset: 0; z-index: 60; pointer-events: none; }
.drawer-root.open { pointer-events: auto; }

.scrim {
  position: absolute; inset: 0;
  background: rgba(17, 22, 31, .34);
  opacity: 0; transition: opacity .28s ease;
}
.drawer-root.open .scrim { opacity: 1; }

.panel {
  position: absolute; inset: 0 auto 0 0;
  display: flex; flex-direction: column;
  width: 82%; max-width: 340px;
  padding-top: var(--safe-top);
  background: var(--bg);
  box-shadow: none;
  transform: translateX(-102%);
  transition: transform .3s cubic-bezier(.22, .68, .19, 1);
}
.drawer-root.open .panel { transform: none; }

.dhead { display: flex; align-items: center; gap: 6px; padding: 10px 10px 6px 12px; }
.sfield {
  flex: 1; display: flex; align-items: center; gap: 7px;
  height: 38px; padding: 0 10px 0 11px;
  border-radius: var(--r-pill); background: var(--fill); color: var(--text-3);
}
.sfield input {
  flex: 1; min-width: 0; border: 0; background: none; outline: none;
  font: inherit; font-size: 14.5px; color: var(--text);
}
.sfield input::placeholder { color: var(--text-3); }
.clr {
  display: grid; place-items: center; width: 18px; height: 18px;
  border: 0; border-radius: 50%; background: var(--border-strong); color: #fff;
}
.newrow {
  display: flex; align-items: center; gap: 10px;
  margin: 2px 10px 4px; padding: 8px 10px;
  border: 0; border-radius: var(--r-md); background: none;
  font-size: 15.5px; font-weight: 600; color: var(--blue);
}
.newrow:active { background: var(--fill); }
.entry-grid {
  display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 5px;
  margin: 0 10px 6px;
}
.entry {
  display: flex; align-items: center; gap: 8px; min-width: 0; min-height: 44px;
  padding: 7px 10px; border: 1px solid var(--border); border-radius: var(--r-md);
  background: var(--bg); color: var(--text-2); text-align: left; font-size: 13px;
}
.entry span { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.entry:active { background: var(--fill-press); color: var(--blue); }
.taskrow {
  display: flex; align-items: center; gap: 10px;
  margin: 0 10px 5px; min-height: 40px; padding: 7px 10px;
  border-radius: var(--r-md); color: var(--text-2); font-size: 14px; text-align: left;
}
.taskrow:active { background: var(--fill); }
.task-count { margin-left: auto; min-width: 22px; padding: 2px 7px; border-radius: var(--r-pill); background: var(--blue); color: #fff; font-size: 11px; text-align: center; }
.nicon {
  display: grid; place-items: center; width: 30px; height: 30px;
  border-radius: 50%; background: var(--blue-soft);
}

.list { flex: 1; overflow-y: auto; padding: 2px 10px 10px; -webkit-overflow-scrolling: touch; }
.empty { margin: 26px 12px; font-size: 13.5px; line-height: 1.8; color: var(--text-3); }

.row {
  display: flex; align-items: center; gap: 4px;
  min-height: 44px; padding: 4px 6px 4px 10px; border-radius: var(--r-md);
}
.row:active { background: var(--fill); }
.row.cur { background: var(--blue-soft); }
.rmain { flex: 1; min-width: 0; }
.rtitle {
  font-size: 14.8px; color: var(--text);
  white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
}
.row.cur .rtitle { color: var(--blue); font-weight: 600; }
.rmeta {
  display: flex; align-items: center; gap: 4px; margin-top: 2px;
  font-size: 11.5px; color: var(--text-3);
}
/* 会话在后台执行中的小圈（放在时间/轮数之后，与 meta 文字同高） */
.rspin {
  flex: none;
  width: 9px; height: 9px; border-radius: 50%;
  border: 2px solid var(--blue-soft);
  border-top-color: var(--blue);
  animation: coomi-rspin 0.9s linear infinite;
}
@keyframes coomi-rspin { to { transform: rotate(360deg); } }
/* 全局常驻会话：生命环图标 + 未读徽标（不用圆点区分） */
.global-row .gicon {
  flex: none; display: grid; place-items: center;
  width: 30px; height: 30px; margin-right: 6px;
  border-radius: 50%; color: var(--accent);
  background: color-mix(in srgb, var(--accent-soft) 60%, var(--bg));
}
.global-badge {
  display: inline-grid; place-items: center;
  min-width: 16px; height: 16px; padding: 0 4px;
  border-radius: 8px; background: var(--accent); color: #fff;
  font-size: 10.5px; font-weight: 650; line-height: 1;
}
.sheet-hint { margin: 0; padding: 12px 14px; color: var(--text-3); font-size: 12px; }
.rmeta {
  display: flex; align-items: center; gap: 4px; margin-top: 3px;
  font-size: 11.5px; color: var(--text-3);
}
.rename {
  width: 100%; padding: 4px 7px;
  border: 1px solid var(--blue-border); border-radius: var(--r-sm);
  background: var(--bg); outline: none;
  font: inherit; font-size: 14.5px; color: var(--text);
}
.rmore {
  display: grid; place-items: center; width: 30px; height: 30px;
  border: 0; border-radius: 50%; background: none; color: var(--text-3);
}

.dfoot { display: flex; align-items: stretch; gap: 8px; border-top: 1px solid var(--border); padding: 8px 10px calc(8px + var(--safe-bottom)); }
.frow.console { color: var(--blue); }
.frow {
  display: flex; align-items: center; gap: 10px; flex: 1; min-width: 0;
  padding: 8px; border: 0; border-radius: var(--r-md); background: none;
}
.frow:active { background: var(--fill); }
.fname { flex: 1; text-align: left; font-size: 15px; font-weight: 600; color: var(--text); }
.fsq {
  display: grid; place-items: center; flex-shrink: 0;
  width: 52px; min-height: 46px;
  border: 0; border-left: 1px solid var(--border);
  border-radius: 0; background: none; color: var(--text-2);
}
.fsq:active { background: var(--fill); }

.sheet-wrap {
  position: absolute; inset: 0; z-index: 2;
  display: flex; align-items: flex-end;
  background: rgba(17, 22, 31, .34);
}
.sheet {
  width: 100%; padding: 4px 10px calc(10px + var(--safe-bottom));
  background: var(--bg); border-radius: 18px 18px 0 0;
  box-shadow: var(--shadow-sheet);
  animation: rise .22s cubic-bezier(.22, .68, .19, 1) both;
}
@keyframes rise { from { transform: translateY(18px); opacity: .5; } to { transform: none; opacity: 1; } }
.sheet-title {
  padding: 12px 12px 8px; font-size: 12.5px; color: var(--text-3);
  white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
}
.sheet-item {
  display: flex; align-items: center; gap: 12px;
  width: 100%; height: 50px; padding: 0 12px;
  border: 0; border-radius: var(--r-md); background: none;
  font-size: 15.5px; color: var(--text);
}
.sheet-item:active { background: var(--fill); }
.sheet-item.danger { color: var(--danger); }
.sheet-cancel {
  width: 100%; height: 48px; margin-top: 6px;
  border: 0; border-radius: var(--r-md); background: var(--fill);
  font-size: 15.5px; font-weight: 600; color: var(--text-2);
}
.token-count { color: var(--blue); font-variant-numeric: tabular-nums; }
</style>
