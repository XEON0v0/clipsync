# 安卓端同步图片自动保存到系统图库 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 所有由 Mac 同步到安卓端的图片自动保存进系统图库 `Pictures/ClipSync` 相册，默认开启、可在"后台"Tab 关闭。

**Architecture:** 在既有汇聚点 `LiveClipboardWriter.imageClip()`（实时推送与 mailbox 历史点按都经过它）内，写剪贴板成功后调用新增的单例组件 `GalleryImageStore`，通过 MediaStore 两段式（IS_PENDING）写入图片；按条目 UUID 生成 `DISPLAY_NAME` 做去重。开关存 SharedPreferences，UI 挂在 MainActivity 的"后台"Tab（PowerPane）。不改 Rust/FFI/协议，不申请新权限。

**Tech Stack:** Kotlin、Jetpack Compose (Material3)、Android MediaStore API、JUnit4 + AndroidX instrumented tests。

**Spec:** `docs/superpowers/specs/2026-08-11-android-save-synced-images-to-gallery-design.md`

## Global Constraints

- 外部开发存储强制：所有 gradle/测试命令前必须 `source scripts/external-dev-env.zsh` 且 `CLIPSYNC_EXTERNAL_READY=1`；gradle 命令带 `--project-cache-dir "$CLIPSYNC_PROJECT_BUILD_ROOT/gradle-project-cache"`；`JAVA_HOME` 缺省用 `/Applications/IntelliJ IDEA.app/Contents/jbr/Contents/Home`（与 `scripts/test-android-core.sh:10` 一致）。
- minSdk 29：写 MediaStore 不需要任何新权限，禁止改 `AndroidManifest.xml`。
- 开关默认值 `true`；相册路径 `Pictures/ClipSync/`；文件名 `clipsync_<uuid>.<ext>`。
- 图库保存失败只能 `Log.w`，禁止向上抛异常（不能影响写剪贴板与 `onClip` 回调结果）。
- UI 文案沿用现有 PowerPane 的硬编码简体中文风格，不进 strings.xml。
- 不引入 ViewModel/DataStore/新依赖。

---

### Task 1: GalleryImageStore 组件 + 开关门控 + 仪器测试

**Files:**
- Modify: `android/app/src/main/java/com/clipsync/app/ClipboardPayload.kt`（新增 `GalleryImageStore`、`imageExtension`，改 `imageClip`）
- Modify: `android/app/src/main/java/com/clipsync/app/AppContract.kt:26-31`（新增 key 与开关读取函数）
- Test: `android/app/src/androidTest/java/com/clipsync/app/PlatformFeasibilityTest.kt`

**Interfaces:**
- Consumes: 现有 `ImagePolicy.mimeType(bytes)`、`ReceivedImageStore.save(context, id, bytes, mimeType)`、`AppContract.PREFERENCES`、`PlatformTestSupport` 测试支撑。
- Produces:
  - `AppContract.KEY_SAVE_TO_GALLERY: String`（值为 `"save_to_gallery"`）
  - `AppContract.saveToGalleryEnabled(context: Context): Boolean`（默认 `true`，Task 2 的 UI 也用它）
  - `GalleryImageStore.save(context: Context, id: String, bytes: ByteArray, mimeType: String): Boolean`

- [ ] **Step 1: 写失败测试**

在 `PlatformFeasibilityTest.kt` 中：

1. 把私有 helper `clipItem`（现有 `:224-229`）改为可传入固定 id：

```kotlin
private fun clipItem(
    content: FfiClipContent,
    id: String = UUID.randomUUID().toString(),
) = FfiClipItem(
    id = id,
    tsMs = System.currentTimeMillis(),
    seq = 1UL,
    content = content,
)
```

2. 在类内新增下列 helper 与三个测试（需要的 imports：`android.os.SystemClock`、`android.provider.MediaStore`）：

```kotlin
private fun samplePngBytes(): ByteArray {
    val bitmap = Bitmap.createBitmap(2, 2, Bitmap.Config.ARGB_8888)
    val bytes = ByteArrayOutputStream().use { output ->
        bitmap.compress(Bitmap.CompressFormat.PNG, 100, output)
        output.toByteArray()
    }
    bitmap.recycle()
    return bytes
}

private fun queryGallery(id: String) = PlatformTestSupport.context.contentResolver.query(
    MediaStore.Images.Media.EXTERNAL_CONTENT_URI,
    arrayOf(MediaStore.Images.Media._ID, MediaStore.Images.Media.RELATIVE_PATH),
    "${MediaStore.Images.Media.DISPLAY_NAME} LIKE ?",
    arrayOf("clipsync_$id.%"),
    null,
)

private fun galleryEntryCount(id: String): Int = queryGallery(id)?.use { it.count } ?: 0

private fun galleryRelativePath(id: String): String? = queryGallery(id)?.use { cursor ->
    if (!cursor.moveToFirst()) return@use null
    val column = cursor.getColumnIndex(MediaStore.Images.Media.RELATIVE_PATH)
    if (column < 0) null else cursor.getString(column)
}

private fun awaitGalleryCount(id: String, expected: Int): Boolean {
    val deadline = SystemClock.uptimeMillis() + 15_000
    while (SystemClock.uptimeMillis() < deadline) {
        if (galleryEntryCount(id) == expected) return true
        SystemClock.sleep(200)
    }
    return false
}

private fun deleteGalleryEntries(id: String) {
    PlatformTestSupport.context.contentResolver.delete(
        MediaStore.Images.Media.EXTERNAL_CONTENT_URI,
        "${MediaStore.Images.Media.DISPLAY_NAME} LIKE ?",
        arrayOf("clipsync_$id.%"),
    )
}

@Test
fun liveImageCallbackSavesImageToGalleryAlbum() {
    PlatformTestSupport.startForegroundService()
    val bytes = samplePngBytes()
    val id = UUID.randomUUID().toString()
    try {
        PlatformTestSupport.app.onClip(clipItem(FfiClipContent.Image(bytes), id))
        assertTrue("gallery entry not found", awaitGalleryCount(id, 1))
        assertTrue(
            "unexpected album: ${galleryRelativePath(id)}",
            galleryRelativePath(id)?.contains("Pictures/ClipSync") == true,
        )
    } finally {
        deleteGalleryEntries(id)
    }
}

@Test
fun sameImageIdSavesToGalleryOnlyOnce() {
    PlatformTestSupport.startForegroundService()
    val bytes = samplePngBytes()
    val id = UUID.randomUUID().toString()
    try {
        val item = clipItem(FfiClipContent.Image(bytes), id)
        PlatformTestSupport.app.onClip(item)
        PlatformTestSupport.app.onClip(item)
        assertTrue("expected exactly one gallery entry", awaitGalleryCount(id, 1))
    } finally {
        deleteGalleryEntries(id)
    }
}

@Test
fun gallerySaveSkippedWhenSwitchDisabled() {
    PlatformTestSupport.preferences.edit()
        .putBoolean(AppContract.KEY_SAVE_TO_GALLERY, false).commit()
    PlatformTestSupport.startForegroundService()
    val bytes = samplePngBytes()
    val id = UUID.randomUUID().toString()
    try {
        PlatformTestSupport.app.onClip(clipItem(FfiClipContent.Image(bytes), id))
        // onClip 同步执行，返回后保存必然已完成或未发生，可直接断言
        assertEquals(0, galleryEntryCount(id))
    } finally {
        deleteGalleryEntries(id)
    }
}
```

- [ ] **Step 2: 验证编译失败（red）**

```bash
source scripts/external-dev-env.zsh
[[ "$CLIPSYNC_EXTERNAL_READY" == 1 ]]
export JAVA_HOME="${JAVA_HOME:-/Applications/IntelliJ IDEA.app/Contents/jbr/Contents/Home}"
android/gradlew --no-daemon -p android \
    --project-cache-dir "$CLIPSYNC_PROJECT_BUILD_ROOT/gradle-project-cache" \
    assembleDebugAndroidTest --console=plain
```

Expected: FAIL — `unresolved reference: KEY_SAVE_TO_GALLERY`（编译期即红，仪器测试的 TDD red 形态）。

- [ ] **Step 3: 实现 AppContract 开关**

`AppContract.kt`：在 `KEY_SERVICE_RUNNING` 一行后加 key，并在 `notificationState()` 后加读取函数：

```kotlin
const val KEY_SAVE_TO_GALLERY = "save_to_gallery"
```

```kotlin
fun saveToGalleryEnabled(context: Context): Boolean =
    context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
        .getBoolean(KEY_SAVE_TO_GALLERY, true)
```

- [ ] **Step 4: 实现 GalleryImageStore 并接入 imageClip**

`ClipboardPayload.kt` 新增 imports：`android.content.ContentResolver`、`android.content.ContentValues`、`android.provider.MediaStore`、`android.util.Log`。

1. 文件内新增共享扩展名函数（放在 `ImagePolicy` 之后），并把 `ReceivedImageStore.save()` 里的 `when (mimeType)` 表达式替换为调用它：

```kotlin
private fun imageExtension(mimeType: String): String = when (mimeType) {
    "image/jpeg" -> "jpg"
    "image/webp" -> "webp"
    "image/gif" -> "gif"
    else -> "png"
}
```

`ReceivedImageStore.save()` 中原来的 `val extension = when (mimeType) { ... }` 改为 `val extension = imageExtension(mimeType)`。

2. 在 `ReceivedImageStore` 之后新增：

```kotlin
object GalleryImageStore {
    private const val TAG = "GalleryImageStore"
    private const val RELATIVE_PATH = "Pictures/ClipSync/"

    fun save(context: Context, id: String, bytes: ByteArray, mimeType: String): Boolean {
        return try {
            writeOnce(context, id, bytes, mimeType)
        } catch (error: Exception) {
            Log.w(TAG, "同步图片保存到图库失败", error)
            false
        }
    }

    private fun writeOnce(context: Context, id: String, bytes: ByteArray, mimeType: String): Boolean {
        val displayName = "clipsync_${UUID.fromString(id)}.${imageExtension(mimeType)}"
        val resolver = context.contentResolver
        if (exists(resolver, displayName)) return true
        val collection = MediaStore.Images.Media.getContentUri(MediaStore.VOLUME_EXTERNAL_PRIMARY)
        val values = ContentValues().apply {
            put(MediaStore.Images.Media.DISPLAY_NAME, displayName)
            put(MediaStore.Images.Media.MIME_TYPE, mimeType)
            put(MediaStore.Images.Media.RELATIVE_PATH, RELATIVE_PATH)
            put(MediaStore.Images.Media.IS_PENDING, 1)
        }
        val uri = resolver.insert(collection, values) ?: return false
        val written = resolver.openOutputStream(uri)?.use { output ->
            output.write(bytes)
            true
        } ?: false
        if (!written) {
            resolver.delete(uri, null, null)
            return false
        }
        values.clear()
        values.put(MediaStore.Images.Media.IS_PENDING, 0)
        resolver.update(uri, values, null, null)
        return true
    }

    private fun exists(resolver: ContentResolver, displayName: String): Boolean {
        val cursor = resolver.query(
            MediaStore.Images.Media.EXTERNAL_CONTENT_URI,
            arrayOf(MediaStore.Images.Media._ID),
            "${MediaStore.Images.Media.DISPLAY_NAME} = ?",
            arrayOf(displayName),
            null,
        ) ?: return false
        return cursor.use { it.moveToFirst() }
    }
}
```

3. `imageClip()`（`:131-143`）在 `ReceivedImageStore.save(...)` 之后插入门控调用：

```kotlin
private fun imageClip(context: Context, id: String, bytes: ByteArray): ClipData {
    val mimeType = ImagePolicy.mimeType(bytes) ?: error("图片格式或尺寸不受支持")
    val file = ReceivedImageStore.save(context, id, bytes, mimeType)
    if (AppContract.saveToGalleryEnabled(context)) {
        GalleryImageStore.save(context, id, bytes, mimeType)
    }
    val uri = FileProvider.getUriForFile(
        context,
        context.packageName + AppContract.FILE_PROVIDER_AUTHORITY_SUFFIX,
        file,
    )
    return ClipData(
        ClipDescription("ClipSync image", arrayOf(mimeType)),
        ClipData.Item(uri),
    )
}
```

- [ ] **Step 5: 在 API 29 模拟器上运行仪器测试（green）**

```bash
source scripts/external-dev-env.zsh
export JAVA_HOME="${JAVA_HOME:-/Applications/IntelliJ IDEA.app/Contents/jbr/Contents/Home}"
android/gradlew --no-daemon -p android \
    --project-cache-dir "$CLIPSYNC_PROJECT_BUILD_ROOT/gradle-project-cache" \
    api29DebugAndroidTest \
    -Pandroid.testInstrumentationRunnerArguments.class=com.clipsync.app.PlatformFeasibilityTest \
    --console=plain
```

Expected: 全部 PASS（含 3 个新用例与既有用例）。managed device 会自动启动 API 29 模拟器。

- [ ] **Step 6: Commit**

```bash
git add android/app/src/main/java/com/clipsync/app/ClipboardPayload.kt \
        android/app/src/main/java/com/clipsync/app/AppContract.kt \
        android/app/src/androidTest/java/com/clipsync/app/PlatformFeasibilityTest.kt
git commit -m "feat(android): auto-save synced images to Pictures/ClipSync gallery album"
```

---

### Task 2: "后台"Tab 开关 UI

**Files:**
- Modify: `android/app/src/main/java/com/clipsync/app/MainActivity.kt`（`PowerPane`，`:450-468`）

**Interfaces:**
- Consumes: `AppContract.KEY_SAVE_TO_GALLERY`、`AppContract.saveToGalleryEnabled(context)`（Task 1 已产出）。
- Produces: 无新接口（纯 UI）。

按设计不做 Compose UI 测试（项目无先例）；本任务的验证是 lint/assemble + 既有仪器测试回归（开关默认值行为已被 Task 1 测试覆盖）。

- [ ] **Step 1: 修改 PowerPane**

新增 imports：`androidx.compose.material3.Switch`、`androidx.compose.ui.platform.LocalContext`。

把 `PowerPane`（`:450-468`）改为：

```kotlin
@Composable
private fun PowerPane(isBatteryExempt: Boolean, requestBatteryExemption: () -> Unit) {
    val context = LocalContext.current
    var saveToGallery by remember { mutableStateOf(AppContract.saveToGalleryEnabled(context)) }
    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(24.dp),
        verticalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        Text("后台接收", style = MaterialTheme.typography.titleLarge)
        Text(if (isBatteryExempt) "电池优化白名单：已允许" else "电池优化白名单：未允许")
        if (!isBatteryExempt) {
            Button(onClick = requestBatteryExemption) { Text("允许后台持续接收") }
        }
        HorizontalDivider()
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Column(Modifier.weight(1f)) {
                Text("保存同步图片到图库", style = MaterialTheme.typography.titleMedium)
                Text(
                    "Mac 同步来的图片自动存入 Pictures/ClipSync",
                    style = MaterialTheme.typography.bodySmall,
                )
            }
            Switch(
                checked = saveToGallery,
                onCheckedChange = { checked ->
                    saveToGallery = checked
                    context.getSharedPreferences(AppContract.PREFERENCES, Context.MODE_PRIVATE)
                        .edit()
                        .putBoolean(AppContract.KEY_SAVE_TO_GALLERY, checked)
                        .apply()
                },
            )
        }
        HorizontalDivider()
        Text("国产 ROM 自启动", style = MaterialTheme.typography.titleMedium)
        Text("小米/红米：手机管家 → 应用管理 → 权限 → 自启动管理")
        Text("华为/荣耀：手机管家 → 应用启动管理 → ClipSync → 手动管理")
        Text("OPPO/一加/realme：设置 → 应用 → 自启动 → ClipSync")
        Text("vivo/iQOO：设置 → 应用与权限 → 权限管理 → 自启动")
    }
}
```

（调用点 `PowerPane(isBatteryExempt, requestBatteryExemption)` 签名不变，`:238` 无需改。）

- [ ] **Step 2: lint + 编译 + 仪器测试回归**

```bash
source scripts/external-dev-env.zsh
export JAVA_HOME="${JAVA_HOME:-/Applications/IntelliJ IDEA.app/Contents/jbr/Contents/Home}"
android/gradlew --no-daemon -p android \
    --project-cache-dir "$CLIPSYNC_PROJECT_BUILD_ROOT/gradle-project-cache" \
    lintDebug testDebugUnitTest api29DebugAndroidTest \
    -Pandroid.testInstrumentationRunnerArguments.class=com.clipsync.app.PlatformFeasibilityTest \
    --console=plain
```

Expected: 全部 PASS，无 lint 错误。

- [ ] **Step 3: Commit**

```bash
git add android/app/src/main/java/com/clipsync/app/MainActivity.kt
git commit -m "feat(android): add gallery auto-save toggle to power tab"
```

---

### Task 3: 双 API 全量回归

**Files:**
- 无代码改动；纯验证。

**Interfaces:**
- Consumes: Task 1、2 的全部产出。

- [ ] **Step 1: 运行项目标准双 API 回归脚本**

```bash
source scripts/external-dev-env.zsh
[[ "$CLIPSYNC_EXTERNAL_READY" == 1 && "$CLIPSYNC_EXTERNAL_LINKS_READY" == 1 ]]
scripts/test-android-core.sh
```

Expected: 结尾输出 `PASS: Android T14 dual-API production validation complete`（脚本依次跑 lint + 单测 + API 29/API 34 模拟器全套仪器测试，`-wipe-data` 干净环境）。

- [ ] **Step 2: 实机/模拟器手动抽查（可选但推荐）**

在有 GUI 的模拟器或真机上安装 debug 包，从 Mac 复制一张图片，确认：
1. 系统图库出现 `Pictures/ClipSync` 相册且包含该图；
2. "后台"Tab 关闭开关后再同步一张，图库无新增。

---

## Self-Review 记录

- Spec 覆盖：GalleryImageStore/去重/两段式写入 → Task 1；开关持久化+读取 → Task 1 Step 3 与 Task 2；三条测试用例 → Task 1 Step 1；双 API 回归 → Task 3。无遗漏。
- 占位符：无 TBD/TODO；所有代码步骤含完整代码。
- 类型一致性：`AppContract.saveToGalleryEnabled(context)`、`AppContract.KEY_SAVE_TO_GALLERY`、`GalleryImageStore.save(context, id, bytes, mimeType)` 在 Task 1 产出、Task 1/2 消费，签名一致；`clipItem` 增加默认参数，既有调用点（无第二参）不受影响。
