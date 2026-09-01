# 外部开发卷去品牌化重命名 实施计划

> **For agentic workers:** 本计划是机器级运维操作（非代码任务），按 Task 顺序逐步执行，checkbox（`- [x]`）跟踪。执行前必须确认"Global Constraints"的停服窗口与命名参数已由用户确认。

**Goal:** 把外部 APFS 开发卷从 ClipSync 专属命名改为通用命名，所有引用同步更新，数据零迁移，验证通过后四个 agent 工具（Codex / Claude Code / Kimi Code / ZCode）与全部工具链照常工作。

**Spec:** 无独立设计文档；引用清单来自 2026-08-22 会话实测摸排（见 Task 1 备份与"引用总账"）。

## 命名参数（执行前由用户确认，以下为默认建议）

| 对象 | 现名 | 建议新名 |
|---|---|---|
| APFS 卷名（挂载点） | `ClipSyncDev` → `/Volumes/ClipSyncDev` | `DevDisk` → `/Volumes/DevDisk` |
| sparse bundle 文件 | `/Volumes/UH100/ClipSyncDev.sparsebundle` | `/Volumes/UH100/DevDisk.sparsebundle` |
| 卷内依赖根目录 | `ClipSyncDeps` | `Deps` |
| 卷内构建根目录 | `ClipSyncBuild` | `Builds` |

卷内 `Developer/`（项目输出、缓存、临时、产物）已是通用名，不动。

## Global Constraints

- **停服窗口**：执行期间无活跃构建、模拟器、simulator、Colima 容器、Gradle/Kotlin daemon；所有终端与 agent 会话在 Task 3 前停止往卷上写（旧 shell 的 `TMPDIR` 指向改名后不存在的路径，会 fail loud 而非静默写内置盘，但仍应避免）。
- **顺序原子性**：Task 3（改名）与 Task 4（改 env.zsh）之间是中间不可用状态，必须连续完成，中间不启动任何开发命令。
- **设备号不写死**：`disk6s1` 等设备标识重启会变，一律用挂载点/卷名解析。
- **备份双份**：配置备份同时放外部卷 `Developer/Backup/volume-rename-20260822/` 和内置盘 `~/.config/external-dev/backup-20260822/`（小配置文件允许内置；防回滚时卷挂载不出来）。
- 仓库当前有未提交修改；本计划会改 `scripts/external-dev-env.zsh` 和 `AGENTS.md`，建议执行前先提交现有工作，避免混合状态。

## 引用总账（实测）

| # | 类别 | 位置 | 处置 |
|---|---|---|---|
| 1 | 配置源头 | `~/.config/external-dev/env.zsh`：`EXTERNAL_DEV_IMAGE`、`EXTERNAL_DEV_VOLUME`、`EXTERNAL_DEV_DEPS_ROOT`、`EXTERNAL_DEV_BUILD_ROOT` 4 个变量 + 自动挂载探测条件里的字面量 `ClipSyncDeps` | Task 4 |
| 2 | 家目录符号链接 ×12 | `~/.gradle` `~/.cargo` `~/.rustup` `~/.colima` `~/.bun` `~/.nuget` `~/.android` `~/.konan` `~/.npm` `~/.m2` `~/.cache` `~/Library/Android/sdk` | Task 5 `ln -sfn` 重指（链接名不变，`docker context` 等引用 `~/.colima` 的东西无感） |
| 3 | 仓库符号链接 ×1 | `macos/Frameworks` → `.../ClipSyncBuild/Frameworks` | Task 5 |
| 4 | AVD ini ×4 | `android-avd/*.ini` 的 `path=/Volumes/ClipSyncDev/...` | Task 6 sed |
| 5 | Colima/Lima ×2 | `~/.colima/_lima/colima/lima.yaml:15`、`colima.yaml:267` 的 mount location | Task 7 |
| 6 | 仓库脚本 ×1 | `scripts/external-dev-env.zsh:60` 硬编码 `ClipSyncBuild` | Task 8 |
| 7 | 活文档 ×2 | 仓库 `AGENTS.md`、`~/.agents/AGENTS.md`（四工具共享 canonical，符号链接） | Task 8 |
| 8 | 历史文档 ×1 | `docs/superpowers/specs/2026-08-11-desktop-app-design.md` | 不改，保留历史语境 |

实测无引用（不需改）：仓库其余全部脚本（走 `$EXTERNAL_DEV_VOLUME` 变量）、`~/.zshenv`、Gradle `init.d`/`gradle.properties`（无绝对路径）、`scripts/audit-external-dev-env.zsh`（只查就绪变量）。

---

### Task 1: 备份与基线记录

- [x] 建两个备份目录：`/Volumes/ClipSyncDev/Developer/Backup/volume-rename-20260822/` 与 `~/.config/external-dev/backup-20260822/`
- [x] 备份 `~/.config/external-dev/env.zsh`、`~/.colima/_lima/colima/{lima,colima}.yaml`、`$ANDROID_AVD_HOME/*.ini` 到两处
- [x] 记录基线：`readlink` 输出 13 个符号链接、`diskutil info /Volumes/ClipSyncDev`、`df -h /Volumes/ClipSyncDev`，存备份目录

### Task 2: 停服

- [x] `adb emu kill`（逐台）或 `pkill -f qemu-system`；`adb kill-server`
- [x] `gradle --stop`（停 Gradle/Kotlin daemon）
- [x] `colima stop`；`xcrun simctl shutdown all`（如有）
- [x] `lsof /Volumes/ClipSyncDev` 确认无残留进程；有则处理后再继续

### Task 3: 改名（连续执行，中间不跑任何开发命令）

- [x] `diskutil rename /Volumes/ClipSyncDev DevDisk`（在线，挂载点立即变 `/Volumes/DevDisk`）
- [x] `diskutil unmount /Volumes/DevDisk`（注意用新名）
- [x] `mv /Volumes/UH100/ClipSyncDev.sparsebundle /Volumes/UH100/DevDisk.sparsebundle`（exFAT 上元数据操作，瞬间完成）
- [x] `hdiutil attach -nobrowse -owners on /Volumes/UH100/DevDisk.sparsebundle` 手动重挂（此时 env.zsh 还指向旧名，不能靠自动挂载）
- [x] `mv /Volumes/DevDisk/ClipSyncDeps /Volumes/DevDisk/Deps`
- [x] `mv /Volumes/DevDisk/ClipSyncBuild /Volumes/DevDisk/Builds`

### Task 4: 改配置源头 env.zsh

- [x] `EXTERNAL_DEV_IMAGE` → `.../DevDisk.sparsebundle`；`EXTERNAL_DEV_VOLUME` → `/Volumes/DevDisk`
- [x] `EXTERNAL_DEV_DEPS_ROOT` → `.../Deps`；`EXTERNAL_DEV_BUILD_ROOT` → `.../Builds`
- [x] 自动挂载探测条件 `[[ ! -d "$EXTERNAL_DEV_VOLUME/ClipSyncDeps"` → `Deps`
- [x] `grep -n 'ClipSync' ~/.config/external-dev/env.zsh` 复核无残留字面量（`CLIPSYNC_*` 兼容变量名除外——那是变量名不是路径，保留）

### Task 5: 重指 13 个符号链接

- [x] 12 个家目录链接逐个 `ln -sfn 新目标 链接名`（例：`ln -sfn /Volumes/DevDisk/Deps/gradle ~/.gradle`）
- [x] `ln -sfn /Volumes/DevDisk/Builds/Frameworks <repo>/macos/Frameworks`
- [x] 逐个 `readlink` 验证目标存在

### Task 6: 修 AVD

- [x] `sed -i '' 's|/Volumes/ClipSyncDev|/Volumes/DevDisk|g' /Volumes/DevDisk/Deps/android-avd/*.ini`
- [x] `grep -rIl '/Volumes/ClipSyncDev' /Volumes/DevDisk/Deps/android-avd/` 扫 `.avd` 目录内文本残留（`-I` 跳过二进制/快照）；config.ini 等如有则同样 sed
- [x] 验证时若快照损坏：`emulator @clipsync-spike-api35 -no-snapshot-load` 启动一次重建

### Task 7: 修 Colima

- [x] （停机状态）sed 两份 yaml 的 mount location：`lima.yaml:15`、`colima.yaml:267`
- [x] `colima start` 后重新 grep 两份 yaml：若 `lima.yaml` 被重渲染覆盖回旧路径，说明它是派生产物——改用 `colima start --mount /Volumes/DevDisk:w` 重新声明挂载并再次验证

### Task 8: 修仓库活文档与脚本

- [x] `scripts/external-dev-env.zsh:60`：`ClipSyncBuild` → `Builds`
- [x] 仓库 `AGENTS.md`：sparsebundle 路径、`/Volumes/ClipSyncDev`、docker bind-mount 例子的挂载点文字
- [x] `~/.agents/AGENTS.md`（canonical）：sparsebundle 名与挂载点文字（改一处，四工具经符号链接同时生效）

### Task 9: 兜底扫描

- [x] 仓库：`grep -rn --exclude-dir=.git -E 'ClipSyncDev|ClipSyncDeps|ClipSyncBuild|UH100/ClipSync' .`（除第 8 项历史文档外应为零命中）
- [x] 家目录配置：`grep -rIl -E '/Volumes/ClipSyncDev' ~/.config ~/.zshenv ~/.zshrc ~/.agents/AGENTS.md 2>/dev/null`
- [x] 卷上小范围：`grep -rIl '/Volumes/ClipSyncDev' /Volumes/DevDisk/Deps/{gradle,android-user} 2>/dev/null`

### Task 10: 验证矩阵

- [x] 新 shell：`EXTERNAL_DEV_READY=1`、`TMPDIR` 指向 `/Volumes/DevDisk/Developer/Temp/`
- [x] 自动挂载路径：`diskutil unmount /Volumes/DevDisk` 后开新 shell，应自动 attach 成功（验证 Task 4 的探测条件改动）
- [x] 13 个符号链接 `readlink -f` 全部可达
- [x] `ls $CARGO_HOME` / `gradle --status`（新 `GRADLE_USER_HOME` 下 daemon 正常）
- [x] `emulator -list-avds` 显示 4 台；headless 启动 `clipsync-spike-api35` 一台成功
- [x] `colima start` + `docker run --rm alpine ls /Volumes`（容器内可见挂载点）
- [x] 仓库 `ls macos/Frameworks/` 可访问
- [x] `source scripts/external-dev-env.zsh`：`CLIPSYNC_EXTERNAL_READY=1` 且 `CLIPSYNC_EXTERNAL_LINKS_READY=1`

### Task 11: 回滚方案（如验证失败）

- [ ] 同样先停服；`diskutil rename` 反向改回卷名、卸载后 `mv` 回 bundle 旧名、`mv` 回 `ClipSyncDeps`/`ClipSyncBuild`
- [ ] 从内置盘备份恢复 `env.zsh`、两份 yaml、4 个 ini
- [ ] `ln -sfn` 按基线记录重指 13 个链接；还原 Task 8 的仓库改动（`git checkout` 或手工）

---

## 自审（Self-Review）

对初稿计划逐条审查后的发现与修正，均已回写进上文：

1. **初版引用清单有盲区（已修正）**：摸排时第一轮 grep 只搜了 `ClipSyncDev|Volumes/UH100`，漏掉 `ClipSyncDeps`/`ClipSyncBuild` 作为独立字符串的引用。补查发现两处硬编码：`scripts/external-dev-env.zsh:60`（`ClipSyncBuild`）和 `env.zsh` 自动挂载探测条件（`ClipSyncDeps`）——漏掉后者会导致"推出卷后新 shell 不再自动挂载"这种难排查的静默故障。兜底扫描（Task 9）已改为四词全量模式。
2. **设备号不可写死（已修正）**：摸排时记录的 `disk6s1` 重启会变，计划中一律用挂载点/卷名解析。
3. **lima.yaml 可能是渲染产物（已加验证分支）**：不确定 `lima.yaml` 与 `colima.yaml` 谁是持久源，Task 7 要求 start 后复检，被覆盖则改走 `colima start --mount` 重声明。
4. **AVD 残留不止 ini（已加扫描）**：`.avd` 目录内 `config.ini` 等可能还有绝对路径，Task 6 用 `grep -I` 扫文本；快照损坏的兜底是 `-no-snapshot-load` 启动一次。
5. **备份单点（已修正）**：备份只放被改名的卷上有循环依赖（回滚时卷可能挂不出），改为外部卷 + 内置盘双份。
6. **旧 shell 环境失效（已声明）**：改名后旧终端的 `TMPDIR` 指向不存在的路径，命令会直接报错（fail loud，不会静默写内置盘），但执行窗口仍要求停止所有活跃会话。
7. **顺序依赖（已固化）**：内部目录 `mv` 必须先于 env.zsh 修改完成；Task 3 手动 `hdiutil attach` 是因为当时 env.zsh 仍指旧 bundle 路径，不能靠自动挂载。
8. **链接名不变是关键不变量（已声明）**：13 个符号链接只改目标不改名，`docker context`、IDE 配置等引用 `~/.colima` 等路径的东西全部无感，无需排查。
9. **兜底扫描防误伤（已限定范围）**：`-I` 只搜文本、限定目录范围，避免扫卷上快照二进制和巨型缓存目录。
10. **git 混合状态（已提示）**：仓库现有未提交修改，本计划再改 2 个文件，建议先提交现有工作。
11. **canonical AGENTS.md 是符号链接（已确认无额外工作）**：四个工具的指令文件都链接到 `~/.agents/AGENTS.md`，Task 8 改一处即全部生效。

**剩余不确定性**：Colima 挂载配置的重渲染行为（Task 7 有分支预案）；AVD `.avd` 内部残留的实际数量（Task 6 有扫描与兜底）。两者都不阻塞计划批准，执行时现场处理。

**工作量重估**：约 30 条命令，人工 45–60 分钟（含一次 headless 模拟器启动与 colima 验证）。

---

## 实施记录（2026-08-22 执行完毕）

Task 1–10 全部完成并验证通过；Task 11（回滚）未执行。执行中的偏差与发现：

1. **Task 2 偏差——本会话 MCP 进程不能杀**：占卷的 node 进程是当前 agent 会话的 MCP 服务器（tavily/mcp-remote，仅持有 npm 调试日志写句柄）。改用 `diskutil unmount force` 处理，进程往失效句柄写日志，无害。另杀掉了 `adb` server 与 `CoreSimulatorService`（后者按需自动重启；其 defaults 中未发现存储的绝对路径）。
2. **Task 3 偏差——强卸后重挂失败**：force unmount 后磁盘镜像仍处于已连接状态，`hdiutil attach` 报"无可装载的文件系统"，`diskutil mount` 亦失败。`hdiutil detach disk6` 后全新 attach 成功。**遗留经验：unmount 只卸卷不卸镜像；attach 前必须先 detach 旧实例。**
3. **Task 6 偏差——首轮 sed 模式不全**：ini 只替换了卷名、漏了 `ClipSyncDeps` 目录名，二次用完整四段替换修复；另清理了 7 个 `emu-launch-params.txt`（每次启动重新生成的运行参数记录）。修复后 AVD 目录树零残留。
4. **Task 7 偏差——Colima 真正的持久配置在 `~/.colima/default/colima.yaml`**（初版清单只扫了 `_lima/colima/`，漏了这个 profile 目录）。只改 `_lima` 下两份时启动 fatal（`lima.yaml` 被从旧源头重渲染，VM `mkdir /Volumes/ClipSyncDev` 被拒）。修 `default/colima.yaml` 后启动成功，`lima.yaml` 正确渲染出 DevDisk，guest 内挂载可写、docker 29.5.2 正常。guest 内 8 月 6 日遗留的 `/Volumes/ClipSyncDev/...` 孤儿挂载点树已删除（含一份 `Caddyfile.local` 孤儿拷贝，原件在仓库 `deploy/Caddyfile.local`，无数据丢失）。验证后 Colima 停回原状态。
5. **Task 9 结论**：残留引用均为有意保留——计划文档自身（本文件）、历史设计文档（计划决定不改）、`~/.config/external-dev/backup-20260822/`（回滚备份）、卷上 gradle 的 daemon 日志 / kotlin profile / 孤儿 `.tmp` 文件（惰性历史数据，不影响功能，未动）。
6. **Task 10 发现——`colima stop` 后 limactl usernet 守护进程残留**并持有卷上日志句柄，导致 eject 被拒（印证"弹出前停 Colima"还需确认 limactl 退出）。杀掉后完整 eject → 新 shell 自动重挂成功（验证了改后的 `/Deps` 探测条件）。
7. **验证矩阵结果**：新 shell `EXTERNAL_DEV_READY=1`、`TMPDIR=/Volumes/DevDisk/Developer/Temp/`；完整弹出后自动重挂 OK；13 个符号链接全部可达；`CLIPSYNC_EXTERNAL_READY=1`、`CLIPSYNC_EXTERNAL_LINKS_READY=1`；Cargo/Gradle 主目录正常；4 台 AVD 可列出，`clipsync-spike-api35` headless 启动约 15 秒完成、adb 在线、guest shell 正常；Colima 启动 + guest 内可写挂载 + docker info 通过。验证后模拟器、adb、Colima 均已停回。

最终状态：卷 `DevDisk`（`/Volumes/DevDisk`，disk6s1）、bundle `/Volumes/UH100/DevDisk.sparsebundle`、内部目录 `Deps/` `Builds/` `Developer/`；回滚备份在外部卷 `Developer/Backup/volume-rename-20260822/` 与内置盘 `~/.config/external-dev/backup-20260822/` 双份。
