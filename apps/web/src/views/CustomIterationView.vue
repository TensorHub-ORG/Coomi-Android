<script setup lang="ts">
import { nextTick, ref } from 'vue'
import { useRouter } from 'vue-router'
import PageHead from '@/components/PageHead.vue'
import CoomiIcon from '@/components/CoomiIcon.vue'
import { apiSend } from '@/bridge/http'
import { goBack } from '@/bridge/navigation'
import { useSessionStore } from '@/stores/session'

const router = useRouter()
const session = useSessionStore()
const busy = ref(false)
const error = ref('')

const PROMPT = `我准备在 Coomi 的手机虚拟 Linux 环境中进行 Coomi 自定义迭代。

开始后，你的第一个任务是先询问我本次使用 GitHub 还是 Gitee 作为开源协作平台（GitHub 仓库：https://github.com/TensorHub-ORG/Coomi-Android；Gitee 仓库：https://gitee.com/tensorhub/coomi），确认后再按所选平台执行下面的步骤。

请先检查本地开发环境：
1. 确认当前命令运行在 Runtime V2 的 Debian ProotLinux 中，源码目录为 ~/custom_coomi，持久 Build Kit 挂载为 /opt/coomi-dev。
2. 这次迭代优先使用所选平台的云端 CI（GitHub Actions / Gitee Go）构建 APK，不要一开始在手机上安装 Android SDK/NDK 或尝试本地编译。
3. 先检查 gh、Git、远程仓库和所选平台 CI 权限；如果缺少 gh 或认证失效，先完成认证并验证，不要把 Token 写入命令、日志或仓库。
4. 本地环境只用于编辑、测试、提交和触发工作流。若需要诊断本地构建环境，再运行 coomidev-build doctor；不要把目录已创建当作工具链已就绪。

然后带我完成所选平台（GitHub 或 Gitee）的环境配置：
1. 引导我注册或登录所选平台的账号（Gitee 仓库为 https://gitee.com/tensorhub/coomi）。
2. 检查并安装 gh CLI（Gitee 使用 git/curl 或 Gitee API）。
3. 使用 gh auth login 的设备码流程完成认证（Gitee 走仓库账号凭证/私人令牌认证）。
4. 教我生成 SSH Key，并将公钥添加到所选平台。
5. 配置 Git 使用 SSH 访问所选平台，并验证 SSH 连接。
6. 如确有需要，再指导我创建平台访问令牌（GitHub Personal Access Token (classic) 或 Gitee 私人令牌），说明最小权限、保存方式和安全注意事项；不要在对话、日志或提交内容中暴露 Token。

配置完成后，请按以下顺序验证：
1. 验证所选平台登录状态和 SSH 连接。
2. 为所选平台的 Coomi 仓库点星（GitHub https://github.com/TensorHub-ORG/Coomi-Android 或 Gitee https://gitee.com/tensorhub/coomi）。
3. Fork 该仓库的 main 分支。
4. 将仓库的 main 分支克隆到 Coomi 虚拟 Linux 环境中的 ~/custom_coomi。
5. 检查仓库、分支、远程地址和工作区状态。

之后，请按照 coomi-custom-iteration Skill 的规则协助我进行自定义开发。每次修改前先检查项目规则、当前分支和工作区状态；完成后运行适当的测试。

构建 CoomiDev APK 时，优先走所选平台的 CI：
1. 在用户自己的 fork 中创建功能分支，提交必要的 Linux 化配置、CI 工作流和代码修改；不得直接提交官方仓库 main。
2. 先检查仓库是否已有 CoomiDev 工作流；没有时，在用户 fork 的 CI 目录（GitHub：.github/workflows/；Gitee：.gitee/workflows/ 或 Gitee Go 流水线）下创建或补齐工作流，使用固定的 Ubuntu runner、JDK/Node/Rust/Android 构建版本，并通过 COOMI_DEV_BUILD=1 构建 CoomiDev。
3. 推送分支后触发工作流并持续等待（GitHub 使用 gh workflow list、gh workflow run、gh run rerun 与 gh run watch --exit-status；Gitee 使用 Gitee Go 的流水线触发），不要只看旧运行记录。
4. 失败时先获取失败步骤日志（GitHub 使用 gh run view --log-failed；Gitee 使用控制台构建日志），区分依赖、架构、签名、缓存和源码错误；修复后重新提交并重新触发，不要盲目重复运行。
5. 成功后下载 APK artifact，校验包名 com.coomidev.android、应用名 CoomiDev、端口 18765、ABI 和签名，再提供给用户安装；PR 和 APK 构建可以是两个独立工作流。
6. 若用户选择发布，先在 fork 的分支完成验收，再按 Skill 规范提交 PR 到所选平台的官方仓库 main（GitHub TensorHub-ORG/Coomi-Android 或 Gitee tensorhub/coomi）。未经用户明确确认，不执行 push、发布 Release 或创建 PR。

若用户明确要求手机本地构建，才将涉及的构建配置 Linux 化：使用 POSIX 路径与命令，移除 npm.cmd、.cmd、Windows 盘符、反斜杠和 windows-x86_64 硬编码，同时保留 Windows 主机检测与环境变量覆盖。依次执行 doctor、Android APK 冒烟测试、Rust/NDK 冒烟测试、完整构建；完整包必须为 CoomiDev、包名 com.coomidev.android、默认端口 18765，并使用 assets/coomi-agent-dev.png。全部通过后再验证包名和签名并导出 APK。如果真实 ARM64 工具链无法验证，立即停止本地构建并回到所选平台的 CI，不要绕过检查。`

async function start() {
  if (busy.value) return
  busy.value = true
  error.value = ''
  try {
    await apiSend('/api/custom-iteration/bootstrap', 'POST', {})
    session.newSession()
    localStorage.setItem(`coomi.draft.${session.sessionId}`, PROMPT)
    await router.push('/')
    await nextTick()
    window.dispatchEvent(new CustomEvent('coomi:prefill-draft', {
      detail: { sessionId: session.sessionId, text: PROMPT },
    }))
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e)
  } finally {
    busy.value = false
  }
}

function goDashboard() { goBack(router, 'dashboard') }
</script>

<template>
  <div class="page">
    <PageHead title="自定义迭代（实验）" @back="goDashboard" />
    <main class="body">
      <section class="hero">
        <h1>全面自定义自己的 CoomiDev</h1>
        <p>Coomi 是由 TensouHub 开源组织维护的 Agent 基座项目。我们非常欣喜地看到，Coomi-Android 收获了很多用户的认可与喜爱。开源生态之所以称为“开源”，也是因为众多开发者始终愿意共建、共享，相互交流、认可。</p>
        <p>为此，我们开放自定义迭代能力，从零开始引导用户迈入开源社区，让每一位爱好者都有机会亲手打造属于自己的 Coomi，同时也能为社区提供独特的创新与改进。</p>
      </section>
      <section class="prep">
        <div class="section-head"><h2>我们为你提供</h2><span>一次准备，持续共建</span></div>
        <div class="feature-list">
            <article class="feature">
            <span class="feature-icon"><CoomiIcon name="shield" :size="17" /></span>
            <span><b>专用迭代 Skill</b><small>内置环境诊断、Linux 化、测试、提交 PR 与 CoomiDev 构建规范</small></span>
          </article>
            <article class="feature">
            <span class="feature-icon"><CoomiIcon name="folder" :size="17" /></span>
            <span><b>隔离开发工作区</b><small><code>~/custom_coomi</code> 与构建工具链独立保存，避免影响主应用</small></span>
          </article>
            <article class="feature">
            <span class="feature-icon"><CoomiIcon name="globe" :size="17" /></span>
              <span><b>Git开源生态社区</b><small>对于用户的所有迭代和改进，Coomi都会提示你，引导你按规范提交PR和封装自己的APK</small></span>
          </article>
        </div>
      </section>
      <section class="auth">
        <h2>开始前你需要准备</h2>
        <p class="note">请先准备 GitHub 或 Gitee 账号。需要提交 PR 时，再配置权限最小化的 Token（GitHub PAT classic / Gitee 私人令牌）；私钥和 Token 只保存在虚拟环境中，不要写入聊天、日志或提交内容。</p>
        <p class="note">进入会话后，Agent 会检查工作区、Linux 工具链和项目规则，再带你选择提交 PR 或构建独立 CoomiDev。</p>
      </section>
      <p v-if="error" class="error">{{ error }}</p>
      <button class="primary" :disabled="busy" @click="start">
        <CoomiIcon v-if="busy" name="refresh" class="spin" :size="17" />
        <CoomiIcon v-else name="arrowRight" :size="17" />
        {{ busy ? '正在准备…' : '去自定义迭代' }}
      </button>
    </main>
  </div>
</template>

<style scoped>
.page { display:flex; flex-direction:column; height:100%; background:var(--page); }
.body { flex:1; overflow-y:auto; padding:16px 14px calc(var(--safe-bottom) + 28px); }
.hero { position:relative; padding:18px 17px 19px; overflow:hidden; border:1px solid var(--border); border-radius:8px; background:var(--bg); box-shadow:var(--shadow-1); }
h1 { margin:0; color:var(--text); font-size:19px; line-height:1.35; }
.hero p { max-width:40em; margin:10px 0 0; color:var(--text-2); font-size:12.8px; line-height:1.75; }
.prep { margin-top:19px; }
.section-head { display:flex; align-items:baseline; justify-content:space-between; gap:10px; margin:0 3px 8px; }
.section-head h2, .auth h2 { margin:0; color:var(--text); font-size:13px; font-weight:650; }
.section-head > span { color:var(--text-3); font-size:10.5px; }
.feature-list { overflow:hidden; border:1px solid var(--border); border-radius:8px; background:var(--bg); }
.feature { display:grid; grid-template-columns:34px minmax(0,1fr); align-items:center; gap:11px; min-height:70px; padding:11px 13px; }
.feature + .feature { border-top:1px solid var(--border); }
.feature-icon { display:grid; place-items:center; width:32px; height:32px; border-radius:7px; background:var(--blue-soft); color:var(--blue); }
.feature:nth-child(2) .feature-icon { background:var(--ok-soft); color:var(--ok); }
.feature:nth-child(3) .feature-icon { background:var(--orange-soft); color:var(--orange); }
.feature > span:last-child { display:flex; min-width:0; flex-direction:column; gap:3px; }
.feature b { color:var(--text); font-size:13.5px; font-weight:650; }
.feature small { color:var(--text-2); font-size:11.5px; line-height:1.5; }
code { font-family:var(--font-mono); font-size:10.8px; }
.auth { margin:19px 3px 0; }
.auth h2 { margin-bottom:7px; }
.note { margin:0; color:var(--text-3); font-size:11.8px; line-height:1.75; }
.error { margin:10px 3px 0; color:var(--danger); font-size:12px; line-height:1.5; }
.primary { display:flex; align-items:center; justify-content:center; gap:7px; width:100%; min-height:44px; margin-top:20px; border:0; border-radius:10px; background:var(--blue); color:#fff; font-size:14px; } .primary:disabled { opacity:.55; }
.spin { animation:spin 1s linear infinite; } @keyframes spin { to { transform:rotate(360deg); } }
</style>
