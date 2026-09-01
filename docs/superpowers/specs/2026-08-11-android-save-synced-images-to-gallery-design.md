# 安卓端同步图片自动保存到系统图库 — 设计

日期：2026-08-11
状态：已与用户对齐，待实施

## 背景与目标

Mac 端通过 ClipSync 把剪贴板图片同步到安卓端。安卓端粘贴图片（content URI 形式的 ClipData）在大量目标 app 中不可用，体验差。目标：**所有由 Mac 同步过来的图片自动保存进安卓系统图库**，用户可直接从图库分享/使用。

已确认的产品决策：

- 默认自动保存，提供开关可关闭。
- 保存位置：`Pictures/ClipSync` 相册（MediaStore `RELATIVE_PATH`）。
- 去重：按同步条目 UUID 去重，同一条目无论应用几次只存一份。
- 开关位置：MainActivity"后台"Tab 加一行 Switch。

## 现状链路（实施前提）

Mac → 安卓的图片接收有两条路径，最终汇聚到同一点：

1. 实时推送：`ClipSyncApp.onClip`（`android/app/src/main/java/com/clipsync/app/ClipSyncApp.kt:270`）→ `LiveClipboardWriter.apply`。
2. 离线 mailbox：用户点历史条目 → `ClipSyncApp.applyHistory` → 同样调 `LiveClipboardWriter.apply`。

汇聚点 `LiveClipboardWriter.imageClip()`（`android/app/src/main/java/com/clipsync/app/ClipboardPayload.kt:131-143`）已有：图片字节、`ImagePolicy.mimeType()` 嗅探出的 mime（`ClipboardPayload.kt:96-110`）、稳定条目 UUID；并通过 `ReceivedImageStore.save()` 落盘 + FileProvider URI 写剪贴板。

环境事实：minSdk 29，写 `MediaStore.Images` 无需任何存储权限，无需改 Manifest。协议不传 mime，接收端嗅探结果足够 MediaStore 使用，**不改 Rust/FFI/协议**。

## 组件设计

### GalleryImageStore（新增）

仿照 `ReceivedImageStore` 的单例（object）风格，新增于 `ClipboardPayload.kt` 中（该文件已含 `ImagePolicy`/`ReceivedImageStore`/`LiveClipboardWriter`，同文件放相关组件是现有习惯）：

- 签名：`fun save(context: Context, itemId: String, bytes: ByteArray, mimeType: String): Boolean`
- 文件名：`clipsync_<itemId>.<ext>`，`ext` 由 mime 映射（`image/png`→png，`image/jpeg`→jpg，`image/webp`→webp，`image/gif`→gif，未知→png）。
- 去重：先 query `MediaStore.Images`，条件 `DISPLAY_NAME = 文件名`，命中任意一条即返回（视为已保存）。query 失败（null cursor / 异常）按"未保存过"处理，直接尝试插入——最坏重复，不丢图。
- 写入两段式：`ContentValues` 带 `RELATIVE_PATH = "Pictures/ClipSync"`、`IS_PENDING = 1` insert → `openOutputStream` 写字节 → `IS_PENDING = 0` update。
- 全路径 try-catch（`SecurityException`/`IOException`/query 异常），失败仅 `Log.w`，返回 `false`；绝不向上抛，不影响写剪贴板主链路。

### 调用点

`LiveClipboardWriter.imageClip()` 在 FileProvider 写剪贴板成功后：

1. 读开关 preference（见下节），关闭则跳过。
2. 开启则调 `GalleryImageStore.save(context, itemId, bytes, mimeType)`，忽略返回值。

因两条接收路径都经 `imageClip()`，实时与 mailbox 历史点按的图片均自动覆盖。

### 开关与 UI

- `AppContract.kt` 新增 `KEY_SAVE_TO_GALLERY`（Boolean），SharedPreferences（`AppContract.PREFERENCES` = `clipsync_state`），默认 `true`，沿用现有 key/读写模式。
- MainActivity"后台"Tab 加一行：Material3 `Switch` + 说明文字"自动保存同步图片到图库（Pictures/ClipSync）"。状态即读即写 SharedPreferences，不引入 ViewModel/监听。
- `imageClip()` 每次收到图片直接读一次 preference；图片为低频事件，不做缓存。

## 错误处理

- 图库保存任何失败都不影响剪贴板写入与 `onClip` 回调结果（不触发 `CoreStatus::Error`）。
- 图片大小/像素校验复用现有 `ImagePolicy`（≤10MiB / ≤50M 像素），校验不过的图片本就进不了 `imageClip()`，不重复检查。

## 测试

- 仪器测试（MediaStore 无法在 JVM 单测运行）：`PlatformFeasibilityTest` 新增用例，借助 `PlatformTestSupport.setClipboardImage`：
  1. 收到图片 clip 后，query `MediaStore.Images` 断言 `Pictures/ClipSync` 下存在 `clipsync_<uuid>.<ext>`；
  2. 同一 id 再次接收，断言仍只有一条；
  3. 开关关闭时接收，断言图库无新增。
- 开关 UI 不做 Compose UI 测试（项目无此先例，保持最小）。
- 回归：`scripts/test-android-core.sh`（lint + 单测 + API 29/34 模拟器全套仪器测试）。

## 明确不做（YAGNI）

- 不改同步协议/Rust/FFI（不加 mime 字段）。
- 不做按内容 hash 去重、不做保存历史/管理页、不做 GIF 特殊处理（按原格式存）。
- 不申请任何新权限（minSdk 29  scoped storage 下不需要）。
