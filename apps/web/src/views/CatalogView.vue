<script setup lang="ts">
/**
 * SKILL / MCP 管理（控制台入口）。
 * 数据来自引擎 /api/catalog；安装走 /api/catalog/{mcp,skills}/install。
 * 交互：点击「安装」→ 弹出确认（名称/描述/来源/生效方式）→ MCP 再填参数，Skill 直接安装。
 */
import { computed, onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import PageHead from '@/components/PageHead.vue'
import CoomiIcon from '@/components/CoomiIcon.vue'
import SearchSettings from '@/components/SearchSettings.vue'
import { authedFetch } from '@/bridge/http'
import { goBack } from '@/bridge/navigation'
import { filterMarketItems } from '@/utils/marketSearch'

const router = useRouter()

/** 解析引擎响应：兼容空 body（旧引擎进程对未知路由返回 404 空体），错误带可读信息。 */
async function parseRes(res: Response): Promise<any> {
  const text = await res.text()
  let data: any = {}
  try { data = text ? JSON.parse(text) : {} } catch { data = {} }
  if (!res.ok) {
    const detail = data.error ?? data.message ?? ''
    throw new Error(detail || `HTTP ${res.status}${text ? '' : '（引擎响应为空，可能版本过旧，请重启应用）'}`)
  }
  return data
}

type Tab = 'mcp' | 'skills'
const tab = ref<Tab>('mcp')
/** 页签内的视图：已安装（本机实际配置，含自建/导入）｜仓库（内置目录）｜市场（社区注册表）。 */
type Scope = 'installed' | 'catalog' | 'market' | 'network'
const scope = ref<Scope>('catalog')

interface RequiredParam { key: string; label: string; secret?: boolean }
interface McpItem {
  id: string; name: string; description: string; transport: string
  required_parameters: RequiredParam[]; installed: boolean; enabled: boolean
  path?: string
}
interface SkillItem { id: string; name: string; description: string; repository: string; installed: boolean; enabled: boolean; path?: string }
/** 社区市场条目（registry.json）：在 SkillItem 基础上扩展元数据与热度来源。 */
interface MarketItem extends SkillItem {
  ref?: string; subdir?: string
  author?: string; tags?: string[]; license?: string; verified?: boolean
}

const mcp = ref<McpItem[]>([])
const skills = ref<SkillItem[]>([])
const installedMcp = ref<McpItem[]>([])
const installedSkills = ref<SkillItem[]>([])
// ── 社区市场（/api/registry）──
const marketSkills = ref<MarketItem[]>([])
const marketMcps = ref<MarketItem[]>([])
const marketStats = ref<{ github?: any; app?: any }>({})
const marketUpdatedAt = ref('')
const marketLoading = ref(false)
const marketQuery = ref('')
const filteredMarketSkills = computed(() => filterMarketItems(marketSkills.value, marketQuery.value))
const filteredMarketMcps = computed(() => filterMarketItems(marketMcps.value, marketQuery.value))
const activeMarketResults = computed(() => {
  if (tab.value === 'skills') return filteredMarketSkills.value
  return filteredMarketMcps.value
})
const marketKindLabel = computed(() => {
  if (tab.value === 'skills') return 'Skills'
  return 'MCP'
})
const marketSearchPlaceholder = computed(() => `搜索 ${marketKindLabel.value}`)
const loading = ref(true)
const error = ref('')
const busy = ref<string | null>(null)
const notice = ref('')
/** 当前展开详情的卡片 id（卡片默认折叠，只显示名称）。 */
const expanded = ref<string | null>(null)
function toggleExpanded(id: string) {
  expanded.value = expanded.value === id ? null : id
}

// ── 安装确认（所有安装必须先确认，不能点击即装）──
const askMcp = ref<McpItem | null>(null)
const askSkill = ref<SkillItem | null>(null)

// ── MCP 安装参数表单（按目录的 required_parameters 动态生成）──
const installingMcp = ref<McpItem | null>(null)
const installValues = ref<Record<string, string>>({})

/** 必填参数是否都已填写（未填完不允许安装，避免 500）。 */
const installReady = computed(() => {
  const item = installingMcp.value
  if (!item) return false
  return item.required_parameters.every(
    p => (installValues.value[p.key] ?? '').trim().length > 0,
  )
})

function confirmMcpInstall(item: McpItem) {
  askMcp.value = item
}

function proceedMcp() {
  const item = askMcp.value
  askMcp.value = null
  if (!item) return
  installingMcp.value = item
  installValues.value = {}
  notice.value = ''
}

function confirmSkillInstall(item: SkillItem) {
  askSkill.value = item
}

// ── 停用 / 启用（管理页卸载 = 停用可恢复；删除 = 彻底删除）──
const askDelete = ref<{ kind: 'mcp' | 'skill'; item: McpItem | SkillItem } | null>(null)

async function setEnabled(kind: 'mcp' | 'skill', item: McpItem | SkillItem, enabled: boolean) {
  busy.value = item.id
  notice.value = ''
  try {
    // 注意：引擎路由为 /api/catalog/{mcp,skills}/...（skills 是复数）
    const resource = kind === 'mcp' ? 'mcp' : 'skills'
    const res = await authedFetch(`/api/catalog/${resource}/${item.id}/enabled`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ enabled }),
    })
    await parseRes(res)
    // 本地乐观更新：立即反映按钮/徽标状态（不依赖后续 load 是否成功）。
    item.enabled = enabled
    notice.value = enabled
      ? `已启用${kind === 'mcp' ? ' MCP' : ' Skill'}「${item.name}」，新开会话后生效`
      : `已停用${kind === 'mcp' ? ' MCP' : ' Skill'}「${item.name}」，文件与配置已保留，可随时重新启用`
    await load()
  } catch (e) {
    notice.value = `${enabled ? '启用' : '停用'}失败：${e instanceof Error ? e.message : String(e)}`
  } finally {
    busy.value = null
  }
}

function confirmDelete(kind: 'mcp' | 'skill', item: McpItem | SkillItem) {
  askDelete.value = { kind, item }
}

async function deleteItem() {
  const target = askDelete.value
  if (!target) return
  busy.value = target.item.id
  notice.value = ''
  try {
    // 注意：引擎路由为 /api/catalog/{mcp,skills}/...（skills 是复数）
    const resource = target.kind === 'mcp' ? 'mcp' : 'skills'
    const res = await authedFetch(`/api/catalog/${resource}/${target.item.id}`, { method: 'DELETE' })
    await parseRes(res)
    notice.value = `已彻底删除${target.kind === 'mcp' ? ' MCP' : ' Skill'}「${target.item.name}」`
    askDelete.value = null
    await load()
  } catch (e) {
    notice.value = `删除失败：${e instanceof Error ? e.message : String(e)}`
  } finally {
    busy.value = null
  }
}

function proceedSkill() {
  const item = askSkill.value
  askSkill.value = null
  if (!item) return
  installSkill(item)
}

function closeInstallForm() {
  installingMcp.value = null
  installValues.value = {}
}

async function load() {
  loading.value = true
  error.value = ''
  try {
    const res = await authedFetch('/api/catalog')
    if (!res.ok) throw new Error(`HTTP ${res.status}`)
    const data = await parseRes(res)
    mcp.value = data.mcp ?? []
    skills.value = data.skills ?? []
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e)
  } finally {
    loading.value = false
  }
}

/** 加载本机已安装列表（含目录之外自建/导入的）：/api/runtime/installed。 */
async function loadInstalled() {
  loading.value = true
  error.value = ''
  try {
    const res = await authedFetch('/api/runtime/installed')
    if (!res.ok) throw new Error(`HTTP ${res.status}`)
    const data = await parseRes(res)
    installedMcp.value = (data.mcp ?? []).map((m: any) => ({
      id: m.id, name: m.name, description: '', transport: m.transport ?? '',
      required_parameters: [], installed: true, enabled: m.enabled, path: m.path,
    }))
    installedSkills.value = (data.skills ?? []).map((s: any) => ({
      id: s.id, name: s.name, description: '', repository: '',
      installed: true, enabled: s.enabled, path: s.path,
    }))
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e)
  } finally {
    loading.value = false
  }
}

// ── MCP Server 可视化配置：列表 / 新建 / 编辑 / 测试 ──
interface McpServerRow {
  name: string; transport: string; enabled: boolean; target: string
  config: Record<string, unknown>; tools_count: number; error?: string | null
}
const serverRows = ref<McpServerRow[]>([])
const serverEditor = ref<{ name: string; editing: boolean; text: string; result: string; busy: boolean } | null>(null)

async function loadServers() {
  try {
    const res = await authedFetch('/api/mcp/servers')
    const data = await parseRes(res)
    serverRows.value = data.servers ?? []
  } catch {
    // 旧引擎没有该接口时静默降级：保留目录安装流程。
    serverRows.value = []
  }
}

function openServerEditor(row?: McpServerRow) {
  serverEditor.value = row
    ? { name: row.name, editing: true, text: JSON.stringify(row.config ?? {}, null, 2), result: '', busy: false }
    : {
        name: '',
        editing: false,
        text: JSON.stringify({ transport: 'stdio', command: '', args: [], env: {} }, null, 2),
        result: '',
        busy: false,
      }
}

function parseServerJson(): Record<string, unknown> | null {
  const editor = serverEditor.value
  if (!editor) return null
  try {
    const value = JSON.parse(editor.text)
    if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('配置必须是 JSON 对象')
    return value as Record<string, unknown>
  } catch (e) {
    editor.result = `JSON 解析失败：${e instanceof Error ? e.message : String(e)}`
    return null
  }
}

async function saveServer() {
  const editor = serverEditor.value
  if (!editor) return
  const config = parseServerJson()
  if (!config) return
  const name = editor.name.trim()
  if (!name) { editor.result = '请填写名称'; return }
  editor.busy = true
  editor.result = ''
  try {
    const res = await authedFetch(
      editor.editing ? `/api/mcp/servers/${encodeURIComponent(name)}` : '/api/mcp/servers',
      {
        method: editor.editing ? 'PUT' : 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name, config }),
      },
    )
    await parseRes(res)
    serverEditor.value = null
    notice.value = `已保存 MCP「${name}」，新开会话后生效`
    await Promise.all([load(), loadInstalled(), loadServers()])
  } catch (e) {
    editor.result = `保存失败：${e instanceof Error ? e.message : String(e)}`
  } finally {
    editor.busy = false
  }
}

async function testServer() {
  const editor = serverEditor.value
  if (!editor) return
  const config = parseServerJson()
  if (!config) return
  const name = editor.name.trim() || 'test'
  editor.busy = true
  editor.result = '正在连接…'
  try {
    const res = await authedFetch(`/api/mcp/servers/${encodeURIComponent(name)}/test`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ config }),
    })
    const data = await parseRes(res)
    const status = data.status ?? {}
    editor.result = status.error
      ? `连接失败：${status.error}`
      : `连接成功，发现 ${status.tools_count ?? 0} 个工具`
  } catch (e) {
    editor.result = `测试失败：${e instanceof Error ? e.message : String(e)}`
  } finally {
    editor.busy = false
  }
}

/** 加载社区市场（/api/registry）：引擎代理拉取注册表 + 热度统计，带 10 分钟缓存。 */
async function loadMarket() {
  marketLoading.value = true
  error.value = ''
  try {
    const res = await authedFetch('/api/registry')
    if (!res.ok) throw new Error(`HTTP ${res.status}`)
    const data = await parseRes(res)
    const remote = data.remote ?? {}
    const installedIds: string[] = data.installed ?? []
    const decorate = (e: any): MarketItem => ({
      id: e.id, name: e.name, description: e.description ?? '',
      repository: e.repository ?? '', ref: e.ref ?? 'main', subdir: e.subdir ?? '',
      author: e.author, tags: e.tags, license: e.license, verified: e.verified,
      installed: installedIds.includes(e.id), enabled: false,
    })
    marketSkills.value = (remote.skills ?? []).map(decorate)
    marketMcps.value = (remote.mcps ?? []).map(decorate)
    marketStats.value = data.stats ?? {}
    marketUpdatedAt.value = remote.updated_at ?? ''
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e)
    marketSkills.value = []
    marketMcps.value = []
  } finally {
    marketLoading.value = false
  }
}

/** 切换视图（已安装/仓库/市场）：已安装与市场首次进入时拉取一次。 */
function switchScope(next: Scope) {
  if (scope.value === next) return
  scope.value = next
  error.value = ''
  if (next === 'installed') { void loadInstalled(); void loadServers() }
  if (next === 'market') void loadMarket()
}

/** 当前视图下要渲染的列表（已安装｜仓库｜市场）。市场条目不进旧模板渲染，
 * 这里仅保证类型完整（transport 等字段填默认值）。 */
const visibleMcp = computed<McpItem[]>(() => {
  if (scope.value === 'installed') return installedMcp.value
  if (scope.value === 'market') {
    return marketMcps.value.map(m => ({
      id: m.id, name: m.name, description: m.description, transport: '',
      required_parameters: [], installed: m.installed, enabled: false,
    }))
  }
  return mcp.value
})
const visibleSkills = computed<SkillItem[]>(() => {
  if (scope.value === 'installed') return installedSkills.value
  if (scope.value === 'market') return marketSkills.value
  return skills.value
})

/** 各类型页签的计数（市场页签在 tab 计数角标上各自独立）。 */
const mcpCount = computed(() => {
  if (scope.value === 'installed') return installedMcp.value.length
  if (scope.value === 'market') return marketMcps.value.length
  return mcp.value.length
})
const skillsCount = computed(() => {
  if (scope.value === 'installed') return installedSkills.value.length
  if (scope.value === 'market') return marketSkills.value.length
  return skills.value.length
})

/** 热度数字格式化：1000+ 显示为 1.0k。 */
function fmt(n: number | undefined | null): string {
  if (n == null) return '—'
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`
  return String(n)
}

/** 市场条目热度：GitHub 公开指标（stars/下载量） + App 安装统计。 */
function heatOf(item: MarketItem) {
  const gh = marketStats.value.github?.skills?.[item.id]
  const app = marketStats.value.app?.events?.install_ok?.[item.id]
  return {
    stars: gh?.stars as number | undefined,
    downloads30d: gh?.downloads_30d as number | undefined,
    install7d: app?.['7d'] as number | undefined,
    installTotal: app?.total as number | undefined,
  }
}

async function installMcp() {
  const item = installingMcp.value
  if (!item) return
  busy.value = item.id
  notice.value = ''
  try {
    const res = await authedFetch('/api/catalog/mcp/install', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ id: item.id, values: installValues.value }),
    })
    await parseRes(res)
    notice.value = `已安装 MCP「${item.name}」，重启引擎或新开会话后生效`
    closeInstallForm()
    await load()
  } catch (e) {
    notice.value = `安装失败：${e instanceof Error ? e.message : String(e)}`
  } finally {
    busy.value = null
  }
}

async function installSkill(item: SkillItem) {
  busy.value = item.id
  notice.value = ''
  try {
    if (scope.value === 'market') {
      // 社区市场条目：按 repository/ref/subdir 直达安装（内置目录不认的 id）。
      const market = item as MarketItem
      const res = await authedFetch('/api/catalog/skills/install-remote', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          id: market.id,
          name: market.name,
          description: market.description,
          repository: market.repository,
          ref: market.ref ?? 'main',
          subdir: market.subdir ?? '',
        }),
      })
      await parseRes(res)
      notice.value = `已安装 Skill「${market.name}」，重启引擎或新开会话后生效`
      await loadMarket()
    } else {
      const res = await authedFetch('/api/catalog/skills/install', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ id: item.id }),
      })
      await parseRes(res)
      notice.value = `已安装 Skill「${item.name}」，重启引擎或新开会话后生效`
      await load()
    }
  } catch (e) {
    notice.value = `安装失败：${e instanceof Error ? e.message : String(e)}`
  } finally {
    busy.value = null
  }
}

onMounted(() => { void load(); void loadServers() })
// 从控制台进入：返回统一回控制台（浏览器环境回聊天主页）
const importError = ref('')
const customMcp = ref(false)
const customMcpName = ref('')
const customMcpConfig = ref('')

async function openImport() {
  if (tab.value === 'mcp') {
    // 自定义 MCP：行内表单（name + JSON config）
    customMcp.value = true
    customMcpName.value = ''
    customMcpConfig.value = ''
    notice.value = ''
    return
  }
  // Skill 文件导入：系统文件选择器，选 zip 由引擎解包安装
  notice.value = '请选择 Skill 包（zip 文件）'
  importError.value = ''
  window.CoomiAndroid?.importFiles?.()
}

async function submitCustomMcp() {
  const name = customMcpName.value.trim()
  let config: any
  try {
    config = customMcpConfig.value.trim() ? JSON.parse(customMcpConfig.value) : { transport: 'stdio', command: '', args: [] }
  } catch {
    importError.value = 'MCP 配置必须是合法 JSON'
    return
  }
  if (!name) { importError.value = '请输入 MCP 名称'; return }
  busy.value = 'custom-mcp'
  notice.value = ''
  importError.value = ''
  try {
    const res = await authedFetch('/api/catalog/mcp/custom', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name, config }),
    })
    await parseRes(res)
    notice.value = `已导入自定义 MCP「${name}」，重启引擎或新开会话后生效`
    customMcp.value = false
    await load()
  } catch (e) {
    importError.value = e instanceof Error ? e.message : String(e)
  } finally {
    busy.value = null
  }
}

function goDashboard() { goBack(router, 'dashboard') }
</script>

<template>
  <div class="page">
    <PageHead title="拓展管理" @back="goDashboard" />
    <main class="body">
      <!-- 一级：已安装 | 仓库 | 市场 -->
      <div class="seg" role="tablist">
        <button class="segitem" :class="{ on: scope === 'installed' }" @click="switchScope('installed')">
          <CoomiIcon name="check" :size="14" />已安装
        </button>
        <button class="segitem" :class="{ on: scope === 'catalog' }" @click="switchScope('catalog')">
          <CoomiIcon name="globe" :size="14" />仓库
        </button>
        <button class="segitem" :class="{ on: scope === 'market' }" @click="switchScope('market')">
          <CoomiIcon name="sparkle" :size="14" />广场
        </button>
        <button class="segitem" :class="{on:scope==='network'}" @click="switchScope('network')"><CoomiIcon name="globe" :size="14" />联网</button>
      </div>
      <SearchSettings v-if="scope==='network'" />
      <template v-else>
      <!-- 二级：MCP | Skills -->
      <div class="tabs">
        <button class="tab" :class="{ on: tab === 'mcp' }" @click="tab = 'mcp'">
          <CoomiIcon name="plug" :size="15" />MCP
          <span class="cnt">{{ mcpCount }}</span>
        </button>
        <button class="tab" :class="{ on: tab === 'skills' }" @click="tab = 'skills'">
          <CoomiIcon name="wrench" :size="15" />Skills
          <span class="cnt">{{ skillsCount }}</span>
        </button>
        <button class="import-btn" type="button" @click="openImport">
          <CoomiIcon :name="tab === 'mcp' ? 'plug' : 'wrench'" :size="14" />
          {{ tab === 'mcp' ? '自定义 MCP' : '导入 Skill 文件' }}
        </button>
      </div>

      <div v-if="customMcp" class="custom-mcp-form">
        <p class="form-title">自定义 MCP</p>
        <input v-model="customMcpName" class="form-input" placeholder="服务器名称，如 my-git" />
        <textarea v-model="customMcpConfig" class="form-input mono" rows="4"
          placeholder='配置 JSON，如 {"transport":"stdio","command":"npx","args":["-y","@modelcontextprotocol/server-git"]} 或 {"transport":"http","url":"https://..."}' />
        <p v-if="importError" class="form-error">{{ importError }}</p>
        <div class="form-actions">
          <button class="btn ghost" @click="customMcp = false">取消</button>
          <button class="btn primary" :disabled="busy !== null" @click="submitCustomMcp">导入</button>
        </div>
      </div>

      <label v-if="scope === 'market'" class="market-search">
        <CoomiIcon name="search" :size="16" />
        <input
          v-model="marketQuery"
          :placeholder="marketSearchPlaceholder"
          :aria-label="marketSearchPlaceholder"
          autocomplete="off"
        />
        <button v-if="marketQuery" type="button" aria-label="清空搜索" @click="marketQuery = ''">
          <CoomiIcon name="close" :size="14" />
        </button>
      </label>

      <a
        v-if="scope === 'market'"
        class="submit-extension"
        href="https://github.com/TensorHub-ORG/coomi-registry/issues/new?template=submission.yml"
        target="_blank"
        rel="noopener noreferrer"
      >
        <span>去提交一个拓展</span>
        <CoomiIcon name="chevronRight" :size="16" />
      </a>

      <p v-if="notice" class="notice" :class="{ err: notice.startsWith('安装失败') }">{{ notice }}</p>
      <p v-if="error" class="notice err">加载失败：{{ error }}</p>
      <p v-if="loading" class="hint">加载中…</p>

      <!-- MCP（市场视图在下方独立渲染） -->
      <template v-if="tab === 'mcp' && scope !== 'market'">
        <div v-if="scope === 'installed'" class="server-bar">
          <button class="act" @click="openServerEditor()">＋ 新建 MCP Server</button>
          <span class="hint">直接编辑 config/mcp_servers.json；env / headers 的值保存后只显示掩码。</span>
        </div>
        <p v-if="!loading && visibleMcp.length === 0" class="hint">
          {{ scope === 'installed' ? '本机还没有已安装的 MCP Server。' : '目录为空，暂时没有可安装的 MCP Server。' }}
        </p>
        <div v-else class="cards">
          <div v-for="item in visibleMcp" :key="item.id" class="card">
            <button class="card-head" @click.stop="toggleExpanded(item.id)">
              <span class="tile" :class="{ on: item.installed }">
                <CoomiIcon name="plug" :size="18" />
              </span>
              <span class="cname">{{ item.name }}</span>
              <span v-if="item.installed" class="badge" :class="item.enabled ? 'ok' : 'off'">
                {{ item.enabled ? '已启用' : '已停用' }}
              </span>
              <span v-else class="badge plain">未安装</span>
              <CoomiIcon name="chevronRight" :size="14" class="chev" :class="{ open: expanded === item.id }" />
            </button>
            <div v-if="expanded === item.id" class="detail">
              <p class="cdesc">{{ item.description || '本机配置的 MCP Server' }}</p>
              <span v-if="item.transport" class="cmeta"><CoomiIcon name="link" :size="12" />{{ item.transport }}</span>
              <span v-if="item.path" class="cmeta path"><CoomiIcon name="folder" :size="12" />{{ item.path }}</span>
              <div class="dops">
                <template v-if="item.installed">
                  <button v-if="serverRows.some(r => r.name === item.name)" class="act" :disabled="busy !== null" @click.stop="openServerEditor(serverRows.find(r => r.name === item.name))">
                    编辑配置
                  </button>
                  <button class="act" :disabled="busy !== null" @click.stop="setEnabled('mcp', item, !item.enabled)">
                    {{ item.enabled ? '停用' : '启用' }}
                  </button>
                  <button class="act danger" :disabled="busy !== null" @click.stop="confirmDelete('mcp', item)">删除</button>
                </template>
                <button v-else class="act" :disabled="busy !== null" @click.stop="confirmMcpInstall(item)">
                  {{ busy === item.id ? '安装中…' : '安装' }}
                </button>
              </div>
            </div>
          </div>
        </div>
      </template>

      <!-- Skills（市场视图在下方独立渲染） -->
      <template v-else-if="scope !== 'market'">
        <p v-if="!loading && visibleSkills.length === 0" class="hint">
          {{ scope === 'installed' ? '本机还没有已安装的 Skill。' : '目录为空，暂时没有可安装的 Skill。' }}
        </p>
        <div v-else class="cards">
          <div v-for="item in visibleSkills" :key="item.id" class="card">
            <button class="card-head" @click.stop="toggleExpanded(item.id)">
              <span class="tile" :class="{ on: item.installed }">
                <CoomiIcon name="wrench" :size="18" />
              </span>
              <span class="cname">{{ item.name }}</span>
              <span v-if="item.installed" class="badge" :class="item.enabled ? 'ok' : 'off'">
                {{ item.enabled ? '已启用' : '已停用' }}
              </span>
              <span v-else class="badge plain">未安装</span>
              <CoomiIcon name="chevronRight" :size="14" class="chev" :class="{ open: expanded === item.id }" />
            </button>
            <div v-if="expanded === item.id" class="detail">
              <p class="cdesc">{{ item.description || '本机安装的 Skill' }}</p>
              <span v-if="item.repository" class="cmeta"><CoomiIcon name="globe" :size="12" />{{ item.repository }}</span>
              <span v-if="item.path" class="cmeta path"><CoomiIcon name="folder" :size="12" />{{ item.path }}</span>
              <div class="dops">
                <template v-if="item.installed">
                  <button class="act" :disabled="busy !== null" @click.stop="setEnabled('skill', item, !item.enabled)">
                    {{ item.enabled ? '停用' : '启用' }}
                  </button>
                  <button class="act danger" :disabled="busy !== null" @click.stop="confirmDelete('skill', item)">删除</button>
                </template>
                <button v-else class="act" :disabled="busy !== null" @click.stop="confirmSkillInstall(item)">
                  {{ busy === item.id ? '安装中…' : '安装' }}
                </button>
              </div>
            </div>
          </div>
        </div>
      </template>

      <!-- 市场（社区注册表）：SKILL 可一键安装；MCP 暂为收录展示 -->
      <template v-else>
        <p v-if="marketLoading" class="hint">加载中…</p>
        <template v-else>
          <p v-if="tab === 'skills' && marketSkills.length === 0" class="hint">
            广场里还没有 SKILL。
          </p>
          <p v-else-if="tab === 'mcp' && marketMcps.length === 0" class="hint">
            广场里还没有 MCP。
          </p>
          <p v-else-if="marketQuery.trim() && activeMarketResults.length === 0" class="hint">
            没有找到“{{ marketQuery.trim() }}”相关的 {{ marketKindLabel }}。
          </p>
          <p v-else-if="tab === 'skills' && marketSkills.length > 0" class="hint sub">
            社区注册表 · 更新于 {{ marketUpdatedAt || '—' }} · 内容托管在贡献者自己的仓库
          </p>

          <div v-if="tab === 'skills' && filteredMarketSkills.length > 0" class="cards">
            <div v-for="item in filteredMarketSkills" :key="item.id" class="card">
              <button class="card-head" @click.stop="toggleExpanded(item.id)">
                <span class="tile" :class="{ on: item.installed }">
                  <CoomiIcon name="wrench" :size="18" />
                </span>
                <span class="cname">{{ item.name }}</span>
                <span v-if="item.verified" class="badge vrf"><CoomiIcon name="shield" :size="10" />已验证</span>
                <span v-if="item.installed" class="badge ok">已安装</span>
                <CoomiIcon name="chevronRight" :size="14" class="chev" :class="{ open: expanded === item.id }" />
              </button>
              <div v-if="expanded === item.id" class="detail">
                <p class="cdesc">{{ item.description || '（无描述）' }}</p>
                <span class="cmeta"><CoomiIcon name="user" :size="12" />{{ item.author || '—' }}</span>
                <a class="cmeta repo" :href="'https://github.com/' + item.repository" target="_blank">
                  <CoomiIcon name="external" :size="12" />{{ item.repository }}
                </a>
                <span class="cmeta"><CoomiIcon name="shield" :size="12" />{{ item.license || '—' }}</span>
                <span v-if="item.tags?.length" class="cmeta"><CoomiIcon name="search" :size="12" />{{ item.tags.join(' · ') }}</span>
                <div class="heat">
                  <span v-if="heatOf(item).stars != null"><CoomiIcon name="sparkle" :size="11" />{{ fmt(heatOf(item).stars) }} stars</span>
                  <span v-if="heatOf(item).install7d != null"><CoomiIcon name="bolt" :size="11" />周安装 {{ heatOf(item).install7d }}</span>
                  <span v-if="heatOf(item).installTotal != null"><CoomiIcon name="check" :size="11" />累计安装 {{ heatOf(item).installTotal }}</span>
                  <span v-if="heatOf(item).downloads30d"><CoomiIcon name="arrowDown" :size="11" />30天下载 {{ fmt(heatOf(item).downloads30d) }}</span>
                </div>
                <div class="dops">
                  <template v-if="item.installed">
                    <button class="act danger" :disabled="busy !== null" @click.stop="confirmDelete('skill', item)">删除</button>
                  </template>
                  <button v-else class="act" :disabled="busy !== null" @click.stop="confirmSkillInstall(item)">
                    {{ busy === item.id ? '安装中…' : '安装' }}
                  </button>
                  <a class="act link" :href="'https://github.com/' + item.repository" target="_blank">查看仓库</a>
                </div>
              </div>
            </div>
          </div>

          <!-- MCP：收录展示，先看仓库（安装支持 v2） -->
          <div v-if="tab === 'mcp' && filteredMarketMcps.length > 0" class="cards">
            <div v-for="item in filteredMarketMcps" :key="item.id" class="card">
              <button class="card-head" @click.stop="toggleExpanded(item.id)">
                <span class="tile"><CoomiIcon name="plug" :size="18" /></span>
                <span class="cname">{{ item.name }}</span>
                <span v-if="item.verified" class="badge vrf"><CoomiIcon name="shield" :size="10" />已验证</span>
                <CoomiIcon name="chevronRight" :size="14" class="chev" :class="{ open: expanded === item.id }" />
              </button>
              <div v-if="expanded === item.id" class="detail">
                <p class="cdesc">{{ item.description || '（无描述）' }}</p>
                <a class="cmeta repo" :href="'https://github.com/' + item.repository" target="_blank">
                  <CoomiIcon name="external" :size="12" />{{ item.repository }}
                </a>
                <div class="dops">
                  <a class="act link" :href="'https://github.com/' + item.repository" target="_blank">查看仓库</a>
                </div>
              </div>
            </div>
          </div>

        </template>
      </template>

      </template>
      <!-- MCP Server 配置编辑（新建 / 编辑 / 测试） -->
      <div v-if="serverEditor" class="sheet-mask" @click.self="serverEditor = null">
        <div class="sheet">
          <div class="grip" />
          <div class="stitle">
            <CoomiIcon name="plug" :size="17" />
            {{ serverEditor.editing ? '编辑' : '新建' }} MCP Server
          </div>
          <label class="editor-field">
            <span>名称</span>
            <input v-model="serverEditor.name" class="form-input" :disabled="serverEditor.editing" placeholder="server-name" autocomplete="off" />
          </label>
          <label class="editor-field">
            <span>配置 JSON</span>
            <textarea v-model="serverEditor.text" class="form-input mono" rows="10" spellcheck="false" />
          </label>
          <p class="sdesc">支持 stdio（command / args / env / cwd）与 http / sse（url / headers）。env、headers 的值保存后只显示掩码；留空表示保持原值。</p>
          <p v-if="serverEditor.result" class="notice" :class="{ err: /失败|错误|解析/.test(serverEditor.result) }">{{ serverEditor.result }}</p>
          <div class="sheet-actions">
            <button class="btn ghost" @click="serverEditor = null">取消</button>
            <button class="btn" :disabled="serverEditor.busy" @click="testServer">测试连接</button>
            <button class="btn primary" :disabled="serverEditor.busy" @click="saveServer">保存</button>
          </div>
        </div>
      </div>

      <!-- 彻底删除确认（管理页卸载 = 停用可恢复，删除 = 彻底删除） -->
      <div v-if="askDelete" class="sheet-mask" @click.self="askDelete = null">
        <div class="sheet">
          <div class="grip" />
          <div class="stitle">
            <CoomiIcon :name="askDelete.kind === 'mcp' ? 'plug' : 'wrench'" :size="17" />
            彻底删除{{ askDelete.kind === 'mcp' ? ' MCP' : ' Skill' }}「{{ askDelete.item.name }}」？
          </div>
          <p class="sdesc">
            {{ askDelete.kind === 'mcp'
              ? '将从 config/mcp_servers.json 中移除该服务（不可恢复）。若只是想暂时不用，请改用「停用」。'
              : '将删除已安装的 Skill 目录与配置记录（不可恢复）。若只是想暂时不用，请改用「停用」。' }}
          </p>
          <div class="sheet-actions">
            <button class="btn ghost" @click="askDelete = null">取消</button>
            <button class="btn danger-solid" :disabled="busy !== null" @click="deleteItem">确认彻底删除</button>
          </div>
        </div>
      </div>

      <!-- MCP 安装确认 -->
      <div v-if="askMcp" class="sheet-mask" @click.self="askMcp = null">
        <div class="sheet">
          <div class="grip" />
          <div class="stitle"><CoomiIcon name="plug" :size="17" />安装 MCP「{{ askMcp.name }}」？</div>
          <p class="sdesc">{{ askMcp.description }}</p>
          <div class="sinfo">
            <span><CoomiIcon name="link" :size="13" />{{ askMcp.transport }}</span>
            <span><CoomiIcon name="folder" :size="13" />写入 config/mcp_servers.json</span>
            <span><CoomiIcon name="refresh" :size="13" />重启引擎或新开会话后生效</span>
          </div>
          <div class="sheet-actions">
            <button class="btn ghost" @click="askMcp = null">取消</button>
            <button class="btn primary" @click="proceedMcp">继续配置</button>
          </div>
        </div>
      </div>

      <!-- Skill 安装确认 -->
      <div v-if="askSkill" class="sheet-mask" @click.self="askSkill = null">
        <div class="sheet">
          <div class="grip" />
          <div class="stitle"><CoomiIcon name="wrench" :size="17" />安装 Skill「{{ askSkill.name }}」？</div>
          <p class="sdesc">{{ askSkill.description }}</p>
          <div class="sinfo">
            <span v-if="askSkill.repository"><CoomiIcon name="globe" :size="13" />{{ askSkill.repository }}</span>
            <span><CoomiIcon name="folder" :size="13" />安装到 skills 目录</span>
            <span><CoomiIcon name="refresh" :size="13" />重启引擎或新开会话后生效</span>
          </div>
          <div class="sheet-actions">
            <button class="btn ghost" @click="askSkill = null">取消</button>
            <button class="btn primary" :disabled="busy !== null" @click="proceedSkill">
              {{ busy === askSkill.id ? '安装中…' : '确认安装' }}
            </button>
          </div>
        </div>
      </div>

      <!-- MCP 安装参数表单 -->
      <div v-if="installingMcp" class="sheet-mask" @click.self="closeInstallForm">
        <div class="sheet">
          <div class="grip" />
          <div class="stitle">配置 {{ installingMcp.name }}</div>
          <p class="sdesc">{{ installingMcp.description }}</p>
          <label v-for="p in installingMcp.required_parameters" :key="p.key" class="field">
            <span>{{ p.label }}<em v-if="!p.secret" class="req">必填</em></span>
            <input
              v-model="installValues[p.key]"
              :type="p.secret ? 'password' : 'text'"
              :placeholder="p.key"
              autocomplete="off"
            />
          </label>
          <p v-if="installingMcp.required_parameters.length === 0" class="sdesc">该 MCP 无需额外配置，直接安装即可。</p>
          <div class="sheet-actions">
            <button class="btn ghost" @click="closeInstallForm">取消</button>
            <button class="btn primary" :disabled="busy !== null || !installReady" @click="installMcp">
              {{ busy === installingMcp.id ? '安装中…' : installReady ? '安装' : '请填写必填项' }}
            </button>
          </div>
        </div>
      </div>
    </main>
  </div>
</template>

<style scoped>
.page { display: flex; flex-direction: column; height: 100%; background: var(--page); }
.body {
  flex: 1; min-height: 0; overflow-y: auto;
  padding: 14px 12px calc(var(--safe-bottom) + 24px);
  -webkit-overflow-scrolling: touch; overscroll-behavior-y: contain;
}
.tabs { display: flex; align-items: center; gap: 8px; margin-bottom: 14px; }
.import-btn {
  margin-left: auto; display: inline-flex; align-items: center; gap: 5px;
  height: 34px; padding: 0 12px; border-radius: var(--r-pill);
  background: var(--blue-soft); color: var(--blue); font-size: 12.5px; font-weight: 650;
}
.custom-mcp-form { margin-bottom: 12px; padding: 12px; border: 1px solid var(--border); border-radius: var(--r-card); background: var(--bg); display: flex; flex-direction: column; gap: 8px; }
.form-title { margin: 0; font-size: 13px; font-weight: 700; color: var(--text); }
.form-input { width: 100%; padding: 9px 10px; border: 1px solid var(--border-strong); border-radius: 8px; background: var(--page); color: var(--text); font-size: 13px; outline: none; }
.form-input.mono { font-family: var(--font-mono); font-size: 11.5px; }
.form-error { margin: 0; font-size: 12px; color: var(--danger); }
.form-actions { display: flex; justify-content: flex-end; gap: 8px; }
.form-actions .btn { min-height: 34px; padding: 0 14px; border-radius: 8px; font-size: 13px; }
.form-actions .btn.primary { background: var(--blue); color: #fff; }
.form-actions .btn.ghost { background: var(--fill); color: var(--text-2); }
.server-bar { display: flex; align-items: center; gap: 8px; margin-bottom: 10px; flex-wrap: wrap; }
.server-bar .hint { margin: 0; font-size: 11.5px; }
.editor-field { display: flex; flex-direction: column; gap: 6px; margin-top: 12px; }
.editor-field > span { font-size: 12px; font-weight: 650; color: var(--text-2); }
.editor-field textarea.form-input { resize: vertical; min-height: 120px; line-height: 1.5; }
.market-search {
  display: flex; align-items: center; gap: 8px;
  height: 42px; margin: -2px 0 12px; padding: 0 12px;
  border-radius: var(--r-card); background: var(--bg); color: var(--text-3);
  box-shadow: var(--shadow-1);
}
.market-search input {
  flex: 1; min-width: 0; border: 0; outline: 0;
  background: none; color: var(--text); font-size: 14px;
}
.market-search input::placeholder { color: var(--text-3); }
.market-search button {
  display: grid; place-items: center; flex-shrink: 0;
  width: 24px; height: 24px; border-radius: 50%;
  background: var(--fill-strong); color: var(--text-2);
}
.submit-extension {
  display: flex; align-items: center; justify-content: space-between;
  min-height: 44px; margin: -2px 0 14px; padding: 0 13px;
  border: 1px solid var(--border); border-radius: var(--r-md);
  background: var(--bg); color: var(--blue); font-size: 13px; font-weight: 650;
  text-decoration: none;
}
.submit-extension:active { background: var(--blue-soft); }
.seg {
  display: flex; gap: 2px; margin-bottom: 12px; padding: 3px;
  border-radius: var(--r-pill); background: var(--fill);
  align-self: flex-start;
}
.segitem {
  display: inline-flex; align-items: center; gap: 5px;
  height: 30px; padding: 0 13px; border: 0; border-radius: var(--r-pill);
  background: none; font-size: 12.5px; font-weight: 600; color: var(--text-3);
}
.segitem.on { background: var(--bg); color: var(--blue); box-shadow: var(--shadow-1); }
.tab {
  flex: 1; display: flex; align-items: center; justify-content: center; gap: 6px;
  min-height: 42px; border-radius: var(--r-md);
  background: var(--fill-strong); color: var(--text-2);
  font-size: 14px; font-weight: 550;
}
.tab.on { background: var(--blue-soft); color: var(--blue); }
.cnt {
  min-width: 18px; height: 18px; padding: 0 5px; border-radius: var(--r-pill);
  display: inline-flex; align-items: center; justify-content: center;
  background: var(--fill); font-size: 11px; font-weight: 650;
}
.tab.on .cnt { background: var(--blue); color: #fff; }

.notice {
  margin: 0 0 12px; padding: 10px 12px; border-radius: var(--r-md);
  background: var(--fill); font-size: 13px; line-height: 1.6; color: var(--text);
}
.notice.err { background: var(--danger-soft, #ffeceb); color: var(--danger, #d43d2e); }
.hint { margin: 18px 0; text-align: center; font-size: 13px; color: var(--text-3); }

.cards { display: flex; flex-direction: column; gap: 8px; }
.card {
  display: flex; flex-wrap: wrap; align-items: center; gap: 10px;
  padding: 10px 12px; border-radius: var(--r-card);
  background: var(--bg); box-shadow: var(--shadow-1);
}
/* 卡片头部：整行可点击，折叠时只显示名称。 */
.card-head {
  display: flex; align-items: center; gap: 10px; flex: 1; min-width: 0;
  padding: 0; border: 0; background: none; text-align: left;
}
.tile {
  flex-shrink: 0; width: 38px; height: 38px; border-radius: 11px;
  display: flex; align-items: center; justify-content: center;
  background: var(--fill-strong); color: var(--text-2);
}
.tile.on { background: var(--blue-soft); color: var(--blue); }
.cname { flex: 1; min-width: 0; font-size: 14.5px; font-weight: 600; color: var(--text); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.badge {
  flex-shrink: 0; padding: 1.5px 8px; border-radius: var(--r-pill);
  font-size: 10.5px; font-weight: 650;
}
.badge.ok { background: var(--ok-soft, #e6f4ea); color: var(--ok, #2e9e5b); }
.badge.off { background: var(--fill); color: var(--text-2); }
.badge.plain { background: var(--fill); color: var(--text-3); }
.badge.vrf { display: inline-flex; align-items: center; gap: 3px; background: var(--blue-soft); color: var(--blue); }
.chev { flex-shrink: 0; color: var(--text-3); transition: transform .18s; }
.chev.open { transform: rotate(90deg); }
.hint.sub { font-size: 11.5px; }
.hint-link { color: var(--blue); font-weight: 600; }
.cmeta.repo { color: var(--blue); }
/* 热度徽章行 */
.heat {
  display: flex; flex-wrap: wrap; gap: 6px 12px; margin-top: 8px;
  font-size: 11px; color: var(--text-3);
}
.heat span { display: inline-flex; align-items: center; gap: 4px; }
.act.link {
  display: inline-flex; align-items: center; justify-content: center; text-decoration: none;
  background: var(--fill-strong); color: var(--text);
}
/* 展开详情：flex-basis 100% 全宽换行，与头部隔开。 */
.detail {
  flex-basis: 100%; min-width: 0;
  display: flex; flex-direction: column; gap: 2px;
  margin-top: 4px; padding-top: 9px; border-top: 1px dashed var(--border);
}
.cdesc {
  margin: 0; font-size: 12.5px; line-height: 1.55; color: var(--text-2);
  word-break: break-word;
}
.cmeta {
  display: inline-flex; align-items: center; gap: 4px; margin-top: 4px;
  font-size: 11px; color: var(--text-3);
}
.cmeta.path { display: block; word-break: break-all; }
.dops { display: flex; gap: 8px; margin-top: 9px; }
.act {
  flex-shrink: 0; min-width: 62px; height: 34px; padding: 0 14px; border-radius: var(--r-pill);
  background: var(--blue); color: #fff; font-size: 13px; font-weight: 600;
}
.act:active { opacity: 0.85; }
.act:disabled { opacity: 0.5; }
.act.danger { background: var(--danger-soft); color: var(--danger); }
.done { flex-shrink: 0; width: 28px; height: 28px; display: flex; align-items: center; justify-content: center; border-radius: 50%; background: var(--ok-soft, #e6f4ea); color: var(--ok, #2e9e5b); }

/* ── 弹层 ── */
.sheet-mask {
  position: fixed; inset: 0; z-index: 90;
  display: flex; align-items: flex-end; justify-content: center;
  background: rgba(0, 0, 0, 0.45);
  padding: 12px;
}
.sheet {
  width: 100%; max-width: 460px; padding: 10px 18px 18px;
  border-radius: var(--r-card); background: var(--bg);
  box-shadow: var(--shadow-2);
}
.grip { width: 36px; height: 4px; margin: 0 auto 12px; border-radius: 2px; background: var(--fill-strong); }
.stitle {
  display: flex; align-items: center; gap: 8px;
  font-size: 15.5px; font-weight: 650; color: var(--text);
}
.sdesc { margin: 8px 0 0; font-size: 12.5px; line-height: 1.65; color: var(--text-2); }
.sinfo {
  display: flex; flex-direction: column; gap: 6px; margin-top: 12px;
  padding: 10px 12px; border-radius: var(--r-md); background: var(--fill);
  font-size: 12px; color: var(--text-2);
}
.sinfo span { display: flex; align-items: center; gap: 6px; }
.field { display: flex; flex-direction: column; gap: 5px; margin-top: 12px; font-size: 12.5px; color: var(--text-2); }
.req { margin-left: 5px; padding: 0 5px; border-radius: 4px; background: var(--blue-soft); color: var(--blue); font-style: normal; font-size: 10px; font-weight: 650; }
.field input {
  height: 42px; padding: 0 12px; border-radius: var(--r-md); border: 1px solid var(--border);
  background: var(--bg-input); color: var(--text); font-size: 15px;
}
.sheet-actions { display: flex; gap: 10px; margin-top: 18px; }
.sheet-actions .btn { flex: 1; }
.btn { min-height: 42px; border-radius: var(--r-md); font-size: 14.5px; font-weight: 600; }
.btn.primary { background: var(--blue); color: #fff; }
.btn.ghost { background: var(--fill-strong); color: var(--text); }
.btn.danger-solid { background: var(--danger, #d43d2e); color: #fff; }
.btn:disabled { opacity: 0.6; }
</style>
