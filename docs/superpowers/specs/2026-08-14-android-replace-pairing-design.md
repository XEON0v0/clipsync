# Android 主动更换配对设计

日期：2026-08-14
状态：已与用户确认，待实施

## 背景与目标

Android 当前只能在未配对状态扫描 Mac 或桌面端显示的二维码。一旦已经配对，配对页只提供发送当前剪贴板，用户无法从 Android 主动切换到另一台电脑。

本功能让 Android 在已配对状态主动发起“更换配对”：用户确认后，Android 清除当前持久化配对，保留本地历史，并立即进入扫描新 Mac 配对码的流程。

已确认的产品决策：

- Android 继续作为扫码方，不生成二维码，不改变现有配对角色。
- 更换配对是一个需要二次确认的破坏性操作。
- 重置成功后自动进入扫码流程，不要求用户再次点击“扫描 Mac 配对码”。
- 本地剪贴板历史和图片缓存不随配对清除。
- 本功能不增加独立的“解除配对”入口。

## 现状与复用边界

Rust Core 已通过 UniFFI 暴露 `CoreHandle.resetPairing()`。该操作会停止当前会话、删除持久化配对并轮换身份密钥，但不会清除历史。macOS 与 Rust 桌面端已经使用同一能力实现解绑或换绑。

Android 当前的相关层次：

- `CoreGateway.kt` 隔离 `CoreHandle`，但尚未暴露重置配对。
- `ClipSyncApp.kt` 在单线程 executor 上串行调用 Core，并通过 `ClipSyncUiState` 发布状态。
- `MainActivity.kt` 的 `PairingPane` 用 Compose 局部变量控制扫码器，已支持相机权限、扫码、SAS 确认和取消。
- `PairingHistoryTest.kt` 与 `PlatformTestSupport.RecordingCoreGateway` 提供真实 `ClipSyncApp` 加假 Gateway 的仪器测试路径。

因此实现仅扩展 Android 宿主层，不修改 Rust Core、UniFFI 生成代码、配对协议或服务端。

## 状态模型

`ClipSyncUiState` 新增两个字段：

- `pairingResetInProgress: Boolean`：表示正在删除当前配对。用于阻止重复请求、禁用冲突操作并显示进度。
- `scannerRequested: Boolean`：表示配对流程要求 UI 打开扫码器。该状态由 `ClipSyncApp` 持有，避免 Activity 重建或 Compose 重组时丢失。

`PairingUiState` 保持现有四态，不加入扫描或重置等宿主 UI 状态：

- `Unpaired`
- `Claiming`
- `SasReady`
- `Paired`

这样 Core 配对快照与 Android UI 辅助状态仍然分离。`ReadyUnpaired` 回调可以刷新 `pairing`，但不会意外清除 `scannerRequested`。

## Gateway 与应用操作

### CoreGateway

`CoreGateway` 增加：

```kotlin
fun resetPairing()
```

`NativeCoreGateway` 直接委托给 `handle.resetPairing()`。不修改生成的 UniFFI Kotlin 绑定，因为该方法已经存在。

### 扫码控制

`ClipSyncApp` 增加：

- `requestPairingScan()`：仅在未配对且未重置时设置 `scannerRequested = true`，同时清除旧 UI 错误。
- `cancelPairingScan()`：设置 `scannerRequested = false`。

现有手动扫码按钮和更换配对成功后的自动扫码都使用这组操作，避免保留两套互相独立的扫码状态。

`claimPairing(qrPayload)` 在进入 `Claiming` 时同步清除 `scannerRequested`。扫码失败后沿用现有逻辑回到 `Unpaired`，用户可再次发起扫码。

### 更换配对

`ClipSyncApp.replacePairing()` 的行为：

1. 仅当当前状态为 `Paired` 且没有正在重置时接受请求。
2. 立即设置 `pairingResetInProgress = true`、`scannerRequested = false`，清除旧错误，并显示“正在更换配对”。
3. 在现有单线程 executor 上调用 `gateway.resetPairing()`。
4. 成功后设置：
   - `pairing = Unpaired`
   - `pairingResetInProgress = false`
   - `scannerRequested = true`
   - `coreStatus = "未配对"`
   - `lastError = null`
5. 失败后设置：
   - `pairingResetInProgress = false`
   - `scannerRequested = false`
   - 从 `gateway.pairingSnapshot()` 读取 Core 的实际状态，不伪造旧的 `Paired` 状态
   - `lastError` 显示“更换配对失败”和具体错误信息

该操作不修改 `history`。Core 的重置契约会在成功或失败后尽量安装新的空 dispatcher 并发出 `ReadyUnpaired`，因此错误返回不代表旧会话仍可继续使用。更换请求使用专门的 executor 任务处理结果与快照刷新，不复用会把部分配对失败统一回退为 `Unpaired` 的通用错误分支。

## Compose 交互

`PairingPane` 改为接收完整 `ClipSyncUiState`，以读取配对、重置进度和扫码请求。

### 已配对状态

已配对区域保留“发送当前剪贴板”，并增加“更换配对”。点击后显示 `AlertDialog`：

- 标题：`更换配对设备？`
- 说明：`当前配对将被移除，本机历史会保留。随后需要扫描新 Mac 的配对码。`
- 确认：`更换并扫码`
- 取消：`取消`

用户取消对话框时不调用 Core。用户确认后关闭对话框并调用 `replacePairing()`。

重置期间：

- 显示 `CircularProgressIndicator` 和“正在移除当前配对”。
- 禁用发送当前剪贴板与更换配对按钮。
- 重复点击或重复调用不会产生第二次重置。

### 扫码状态

扫码器显示条件改为：

```text
scannerRequested && cameraGranted && pairing == Unpaired
```

当 `scannerRequested` 为真但尚未获得相机权限时，Compose 触发一次权限请求：

- 授权：保持扫码请求并显示 `QrScannerView`。
- 拒绝：调用 `cancelPairingScan()`，显示现有相机权限错误，并停留在未配对页。

扫描成功时先由 `claimPairing()` 清除扫码请求，再进入现有 `Claiming -> SasReady -> Paired` 流程。用户点击“取消扫码”时调用 `cancelPairingScan()`。

更换配对已经成功但相机权限被拒绝时，旧配对不会恢复。用户仍处于未配对状态，可从系统设置授权后再次点击手动扫码。

## 并发与回调

- Core 的所有操作继续在 `clipsync-android-core` 单线程 executor 上执行。
- `pairingResetInProgress` 是应用状态中的互斥门，避免多次重置排队。
- `scannerRequested` 独立于 Core 快照。Core 发出的 `ReadyUnpaired` 只更新连接与配对状态，不取消已经发出的扫码请求。
- 重置期间到达的旧会话状态回调可以更新状态文字，但不能重新建立已删除的持久化配对。
- 应用关闭时沿用现有 generation 与 shutdown 机制，不新增线程或协程作用域。

## 错误处理

- Gateway 不可用：更换失败，保持调用前的 UI，并显示“同步服务尚未就绪”。
- Core 重置失败：读取并展示 Core 的实际配对快照，不启动自动扫码；通常会进入 `Unpaired`，用户可在确认错误后手动重试扫码。
- 相机权限拒绝或扫码器失败：只结束扫码，不回滚已成功完成的重置。
- SAS 不一致、配对码无效和用户取消继续使用现有配对错误处理。

## 测试设计

实施必须遵循测试先行。

### 应用与 Gateway 仪器测试

扩展 `RecordingCoreGateway`，支持：

- 记录 `resetPairing()` 调用次数。
- 成功重置时把假 Gateway 的配对状态改为 `Unpaired`。
- 可配置的重置失败。
- 保留已有假历史，验证配对重置不会清空历史。

在 `PairingHistoryTest` 添加：

1. `replacePairingResetsCurrentPairingAndRequestsScanner`
   - 从 `Paired` 开始。
   - 调用 `replacePairing()`。
   - 断言只调用一次重置、最终为 `Unpaired`、`scannerRequested = true`、进度结束。
2. `replacePairingPreservesLocalHistory`
   - 重置前准备历史。
   - 断言重置后历史条目不变。
3. `replacePairingFailureUsesActualCoreSnapshot`
   - 模拟 Gateway 重置失败。
   - 断言使用 Gateway 返回的实际快照、未请求扫码、进度结束且错误可见。
4. `duplicateReplacePairingRequestRunsOneReset`
   - 在第一次重置尚未完成时再次调用。
   - 断言 Gateway 只收到一次重置。

### Compose 行为测试

为配对页提取可测试的包内可见 Composable 或通过现有 MainActivity 仪器测试验证：

- “更换配对”先显示确认对话框，取消不会调用重置。
- 确认后显示重置进度并禁用冲突操作。
- 重置成功且已授权相机后进入扫码器。

相机实际识码能力由现有 QR 扫码流程覆盖，本功能不重复测试 ML Kit。

### 回归验证

所有构建和测试先运行外置开发盘预检。验证顺序：

1. Android `testDebugUnitTest` 与 `lintDebug`。
2. `assembleDebug` 与 `assembleDebugAndroidTest`。
3. 在已有外置 AVD 上单独运行 `PairingHistoryTest`。
4. 运行 `scripts/test-android-core.sh` 的 API 29/34 完整回归。

## 明确不做

- 不让 Android 生成配对二维码或成为 offerer。
- 不修改 Rust Core、UniFFI、协议或 relay 服务端。
- 不增加独立“解除配对”入口。
- 不清理历史或图片缓存；身份密钥轮换沿用 Core 的既有安全契约。
- 不增加多设备管理、设备列表或自动选择最近设备。
