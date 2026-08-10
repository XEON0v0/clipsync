# Android 应用 M3 Expressive 视觉重构实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 ClipSync Android 应用的全部 UI 重构为 Android 15 原生观感：Material 3 Expressive 形状 + 弹簧动效、Material You 动态取色、跟随系统深浅色、targetSdk 35 + edge-to-edge 沉浸。

**Architecture:** 新建单一主题入口 `ui/theme/Theme.kt` 的 `ClipSyncTheme`（内部使用 `MaterialExpressiveTheme`，形状/动效用 Expressive 默认值，颜色按 API 31+ 动态取色、低版本回落 M3 基线紫色板），三个界面（`MainActivity`、`FocusClipboardActivity`、`QrScannerView` 所在的配对页）统一接入；历史列表行从「纯行 + 分隔线」改为圆角 `Card`；`FocusClipboardActivity` 从 View TextView 转为 Compose。

**Tech Stack:** Jetpack Compose（BOM 2026.05.01 / Compose 1.11.x）、androidx.compose.material3 **1.5.0-alpha21**（显式 pin，见 Task 1 原因）、Kotlin 2.2.21、AGP 8.8.2、Gradle 8.13、compileSdk 36、targetSdk 35、minSdk 29。

> 路线决策（2026-08-10 用户确认）：1.5.0-alpha24 的传递依赖 compose 1.12.0-beta01 硬性要求 compileSdk ≥ 37 且 AGP ≥ 9.1.0，超出本计划范围。改用 alpha21（有真实项目验证其配 BOM 2026.05.01 可构建）+ compileSdk 36，AGP/Gradle 不动。

## Global Constraints

- 所有 Gradle/构建/模拟器命令之前必须先 `source scripts/external-dev-env.zsh`，且 `CLIPSYNC_EXTERNAL_READY=1`；一切构建输出走外部卷（仓库根 `build.gradle.kts` 已按 `CLIPSYNC_ANDROID_BUILD_ROOT` 重定向，禁止改回内盘）。
- minSdk 保持 29 不变；动态取色仅 API 31+，低版本用主题文件内的静态回落色板。
- 组件**不单独覆盖 shape**——圆角全部来自 `MaterialExpressiveTheme` 的 Expressive 默认 `Shapes()`。
- 不新增任何截图测试/新测试框架依赖；验证 = 编译 + lint + 现有测试 + 模拟器截图人工核对。
- 所有 Expressive API 调用点需要 `@OptIn(ExperimentalMaterial3ExpressiveApi::class)`。
- 现有中文 UI 文案、`strings.xml`、业务逻辑（配对/历史/后台服务）一律不动。
- 深色模式用 `isSystemInDarkTheme()` 跟随系统，不加应用内开关。
- commit 只在每个 Task 末尾做，message 用英文 conventional commit。

---

### Task 1: 工具链与依赖升级（Kotlin / SDK 35 / Compose BOM / material3 pin）

**Files:**
- Modify: `android/build.gradle.kts`（插件版本）
- Modify: `android/app/build.gradle.kts`（compileSdk、targetSdk、BOM、material3 pin）

**Interfaces:**
- Consumes: 无（首个任务）
- Produces: 依赖中存在 `androidx.compose.material3:material3:1.5.0-alpha21`（提供 `MaterialExpressiveTheme` 与 `MotionScheme`，供 Task 2 使用）；compileSdk = 36、targetSdk = 35。

> 为什么 pin alpha：BOM 2026.05.01 映射的 material3 稳定版是 1.4.0，其中 `MaterialExpressiveTheme` 未公开（仍为 internal）；公开该 API 需要 1.5.0-alpha 系列。选用 alpha21：有真实项目（2026 年 5–6 月）验证其配 BOM 2026.05.01 可构建；更新的 alpha（如 alpha24）传递依赖 compose 1.12.0-beta，硬性要求 compileSdk ≥ 37 且 AGP ≥ 9.1.0，已排除。

- [ ] **Step 1: 升级 Kotlin 插件版本**

`android/build.gradle.kts` 中把两处 `2.0.21` 改为 `2.2.21`：

```kotlin
plugins {
    id("com.android.application") version "8.8.2" apply false
    id("org.jetbrains.kotlin.android") version "2.2.21" apply false
    id("org.jetbrains.kotlin.plugin.compose") version "2.2.21" apply false
}
```

- [ ] **Step 2: 升级 SDK 与 Compose 依赖**

`android/app/build.gradle.kts` 中：

```kotlin
android {
    namespace = "com.clipsync.app"
    compileSdk = 36

    defaultConfig {
        // ...
        targetSdk = 35
        // 其余不变
    }
}
```

```kotlin
dependencies {
    val composeBom = platform("androidx.compose:compose-bom:2026.05.01")
    implementation(composeBom)
    androidTestImplementation(composeBom)

    // material3 显式 pin 到 1.5.0-alpha21，覆盖 BOM 的 1.4.0，
    // 以获得公开的 MaterialExpressiveTheme / MotionScheme API
    implementation("androidx.compose.material3:material3:1.5.0-alpha21")
    // 其余依赖保持不变（activity-compose 1.9.1 已含 enableEdgeToEdge）
}
```

- [ ] **Step 3: AGP 8.8.2 放行 compileSdk 36，并确保 android-36 平台就位**

AGP 8.8.2 官方支持到 compileSdk 35，编译 36 需要显式放行：在 `android/gradle.properties` 追加一行：

```properties
android.suppressUnsupportedCompileSdk=36
```

确认外部卷 SDK 已安装 android-36 平台，没有则安装（平台必须落在外部卷 SDK，禁止装到内盘）：

```bash
source scripts/external-dev-env.zsh
[[ -d "$ANDROID_HOME/platforms/android-36" ]] || \
  "$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager" "platforms;android-36"
```

（外部卷 SDK 目前装有 android-29/34/35/36.1；36.1 与 36 不是同一目录，以本步检查为准。）

- [ ] **Step 4: 构建验证依赖解析**

```bash
source scripts/external-dev-env.zsh
[[ "$CLIPSYNC_EXTERNAL_READY" == 1 ]]
android/gradlew --no-daemon -p android \
  --project-cache-dir "$CLIPSYNC_PROJECT_BUILD_ROOT/gradle-project-cache" \
  :app:assembleDebug --console=plain
```

Expected: BUILD SUCCESSFUL。

可能的失败与处置（按报错原文对号）：
- 报错仍要求 compileSdk ≥ 37 或 AGP ≥ 9.1.0：把 material3 pin 降到 `1.5.0-alpha13`（2026-01-28，早于 compose 1.12 传递依赖）重跑本步；若 alpha13 仍报同样的错，停止并上报 BLOCKED（不要再往下降，也不要动 AGP/Gradle）。
- 报错 Compose 编译器与 Kotlin 版本不匹配：按报错中要求的最低版本号上调 `android/build.gradle.kts` 两处 Kotlin 插件版本，再重跑。

- [ ] **Step 5: Commit**

```bash
git add android/build.gradle.kts android/app/build.gradle.kts android/gradle.properties
git commit -m "build(android): bump Kotlin 2.2.21, compileSdk 36, Compose BOM 2026.05.01, pin material3 1.5.0-alpha21"
```

---

### Task 2: 新建共享主题 `ClipSyncTheme`

**Files:**
- Create: `android/app/src/main/java/com/clipsync/app/ui/theme/Theme.kt`

**Interfaces:**
- Consumes: Task 1 的 material3 1.5.0-alpha21。
- Produces: `@Composable fun ClipSyncTheme(content: @Composable () -> Unit)`，包名 `com.clipsync.app.ui.theme`——Task 3、Task 5 都包它。

- [ ] **Step 1: 写主题文件**

创建 `android/app/src/main/java/com/clipsync/app/ui/theme/Theme.kt`，完整内容：

```kotlin
package com.clipsync.app.ui.theme

import android.os.Build
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.ExperimentalMaterial3ExpressiveApi
import androidx.compose.material3.MaterialExpressiveTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext

// M3 基线紫色板，仅作为 API < 31（无动态取色）时的回落
private val FallbackLight = lightColorScheme(
    primary = Color(0xFF6750A4),
    onPrimary = Color(0xFFFFFFFF),
    primaryContainer = Color(0xFFEADDFF),
    onPrimaryContainer = Color(0xFF21005D),
    secondary = Color(0xFF625B71),
    onSecondary = Color(0xFFFFFFFF),
    secondaryContainer = Color(0xFFE8DEF8),
    onSecondaryContainer = Color(0xFF1D192B),
    tertiary = Color(0xFF7D5260),
    onTertiary = Color(0xFFFFFFFF),
    tertiaryContainer = Color(0xFFFFD8E4),
    onTertiaryContainer = Color(0xFF31111D),
    error = Color(0xFFB3261E),
    onError = Color(0xFFFFFFFF),
    errorContainer = Color(0xFFF9DEDC),
    onErrorContainer = Color(0xFF410E0B),
    background = Color(0xFFFEF7FF),
    onBackground = Color(0xFF1D1B20),
    surface = Color(0xFFFEF7FF),
    onSurface = Color(0xFF1D1B20),
    surfaceVariant = Color(0xFFE7E0EC),
    onSurfaceVariant = Color(0xFF49454F),
    outline = Color(0xFF79747E),
)

private val FallbackDark = darkColorScheme(
    primary = Color(0xFFD0BCFF),
    onPrimary = Color(0xFF381E72),
    primaryContainer = Color(0xFF4F378B),
    onPrimaryContainer = Color(0xFFEADDFF),
    secondary = Color(0xFFCCC2DC),
    onSecondary = Color(0xFF332D41),
    secondaryContainer = Color(0xFF4A4458),
    onSecondaryContainer = Color(0xFFE8DEF8),
    tertiary = Color(0xFFEFB8C8),
    onTertiary = Color(0xFF492532),
    tertiaryContainer = Color(0xFF633B48),
    onTertiaryContainer = Color(0xFFFFD8E4),
    error = Color(0xFFF2B8B5),
    onError = Color(0xFF601410),
    errorContainer = Color(0xFF8C1D18),
    onErrorContainer = Color(0xFFF9DEDC),
    background = Color(0xFF141218),
    onBackground = Color(0xFFE6E0E9),
    surface = Color(0xFF141218),
    onSurface = Color(0xFFE6E0E9),
    surfaceVariant = Color(0xFF49454F),
    onSurfaceVariant = Color(0xFFCAC4D0),
    outline = Color(0xFF938F99),
)

@OptIn(ExperimentalMaterial3ExpressiveApi::class)
@Composable
fun ClipSyncTheme(content: @Composable () -> Unit) {
    val darkTheme = isSystemInDarkTheme()
    val colorScheme = when {
        Build.VERSION.SDK_INT >= Build.VERSION_CODES.S -> {
            val context = LocalContext.current
            if (darkTheme) dynamicDarkColorScheme(context) else dynamicLightColorScheme(context)
        }
        darkTheme -> FallbackDark
        else -> FallbackLight
    }
    // shapes / motionScheme / typography 刻意不传：
    // MaterialExpressiveTheme 的默认值即 Expressive 大圆角 + 弹簧动效
    MaterialExpressiveTheme(colorScheme = colorScheme, content = content)
}
```

- [ ] **Step 2: 编译验证**

```bash
source scripts/external-dev-env.zsh
android/gradlew --no-daemon -p android \
  --project-cache-dir "$CLIPSYNC_PROJECT_BUILD_ROOT/gradle-project-cache" \
  :app:compileDebugKotlin --console=plain
```

Expected: BUILD SUCCESSFUL。

可能的失败与处置：
- `Unresolved reference: MaterialExpressiveTheme`：改用等价写法 `MaterialTheme(colorScheme = colorScheme, motionScheme = MotionScheme.expressive(), shapes = Shapes(), content = content)`（`MaterialTheme` 的 `motionScheme` 形参同样需要本文件已有的 `@OptIn`；需新增 import `androidx.compose.material3.MaterialTheme`、`androidx.compose.material3.MotionScheme`、`androidx.compose.material3.Shapes`），再重跑。

- [ ] **Step 3: Commit**

```bash
git add android/app/src/main/java/com/clipsync/app/ui/theme/Theme.kt
git commit -m "feat(android): add ClipSyncTheme with M3 Expressive defaults and dynamic color"
```

---

### Task 3: MainActivity 接入主题 + edge-to-edge + 系统栏 insets

**Files:**
- Modify: `android/app/src/main/java/com/clipsync/app/MainActivity.kt`（onCreate、ClipSyncScreen、PairingPane 扫码覆盖层）

**Interfaces:**
- Consumes: Task 2 的 `com.clipsync.app.ui.theme.ClipSyncTheme`。
- Produces: 主界面沉浸式、内容不被状态栏/导航栏遮挡；`ClipSyncScreen`/`PairingPane`/`HistoryPane` 等签名不变。

- [ ] **Step 1: onCreate 启用 edge-to-edge 并换主题**

`MainActivity.kt` 新增 import：

```kotlin
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.material3.Scaffold
import com.clipsync.app.ui.theme.ClipSyncTheme
```

`onCreate` 中，`setContent` 之前加一行，并把 `MaterialTheme {` 换成 `ClipSyncTheme {`：

```kotlin
override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    enableEdgeToEdge()
    setContent {
        val uiState by app.uiState.collectAsState()
        ClipSyncTheme {
            Surface(modifier = Modifier.fillMaxSize()) {
                // ClipSyncScreen(...) 调用不变
            }
        }
    }
    // ...
}
```

- [ ] **Step 2: ClipSyncScreen 根布局改 Scaffold 承接 insets**

`ClipSyncScreen` 现在的根是一个 `Column(modifier = Modifier.fillMaxSize())`。改为：

```kotlin
Scaffold(modifier = Modifier.fillMaxSize()) { innerPadding ->
    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(innerPadding),
    ) {
        // 原有的头部 Column（ClipSync 标题/状态）、TabRow、
        // when (selectedTab) { ... } 全部原样保留，一行不改
    }
}
```

需要新增 import `androidx.compose.foundation.layout.padding` 已无（文件里有），`Scaffold` 在 Step 1 已 import。`innerPadding` 类型是 `PaddingValues`，无需额外 import。

- [ ] **Step 3: 扫码覆盖层取消按钮避开导航栏**

`PairingPane` 中扫码覆盖层的 `OutlinedButton`（"取消扫码"），modifier 由

```kotlin
modifier = Modifier
    .align(Alignment.BottomCenter)
    .padding(24.dp),
```

改为

```kotlin
modifier = Modifier
    .align(Alignment.BottomCenter)
    .navigationBarsPadding()
    .padding(24.dp),
```

- [ ] **Step 4: 编译 + lint 验证**

```bash
source scripts/external-dev-env.zsh
android/gradlew --no-daemon -p android \
  --project-cache-dir "$CLIPSYNC_PROJECT_BUILD_ROOT/gradle-project-cache" \
  :app:lintDebug :app:assembleDebug --console=plain
```

Expected: BUILD SUCCESSFUL，无新增 lint error。

- [ ] **Step 5: Commit**

```bash
git add android/app/src/main/java/com/clipsync/app/MainActivity.kt
git commit -m "feat(android): apply ClipSyncTheme and edge-to-edge insets in MainActivity"
```

---

### Task 4: 历史列表圆角卡片化 + 缩略图圆角

**Files:**
- Modify: `android/app/src/main/java/com/clipsync/app/MainActivity.kt`（HistoryPane、HistoryRow）

**Interfaces:**
- Consumes: Task 3 之后 `MaterialTheme.shapes` 已是 Expressive 默认 shape 表。
- Produces: 历史项以圆角 `Card` 呈现；`HistoryRow(item: CoreHistoryItem, app: ClipSyncApp)` 签名不变。

- [ ] **Step 1: HistoryPane 的 LazyColumn 改为卡片流**

`HistoryPane` 中现有的：

```kotlin
LazyColumn(modifier = Modifier.fillMaxSize()) {
    items(state.history, key = CoreHistoryItem::id) { item ->
        HistoryRow(item, app)
        HorizontalDivider()
    }
}
```

改为（去掉 `HorizontalDivider`，加卡片间距与边距）：

```kotlin
LazyColumn(
    modifier = Modifier.fillMaxSize(),
    contentPadding = PaddingValues(horizontal = 16.dp, vertical = 8.dp),
    verticalArrangement = Arrangement.spacedBy(8.dp),
) {
    items(state.history, key = CoreHistoryItem::id) { item ->
        HistoryRow(item, app)
    }
}
```

新增 import `androidx.compose.foundation.layout.PaddingValues`。注意：文件里剩下的另一处 `HorizontalDivider`（PowerPane 中）保留不动。

- [ ] **Step 2: HistoryRow 包进 Card**

`HistoryRow` 现有的根 `Row(...)` 外包一层 `Card`（不传 shape，继承主题默认）：

```kotlin
Card(modifier = Modifier.fillMaxWidth()) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable { app.applyHistory(item.id) }
            .padding(horizontal = 20.dp, vertical = 12.dp),
        horizontalArrangement = Arrangement.spacedBy(14.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        // when (val content = item.content) { ... } 与末尾时间戳 Column 原样保留
    }
}
```

新增 import `androidx.compose.material3.Card`。

- [ ] **Step 3: 历史图片缩略图加圆角**

`HistoryRow` 中图片分支的 `Image`，modifier 由 `Modifier.size(64.dp)` 改为：

```kotlin
modifier = Modifier
    .size(64.dp)
    .clip(MaterialTheme.shapes.medium),
```

新增 import `androidx.compose.ui.draw.clip`。

- [ ] **Step 4: 编译验证**

```bash
source scripts/external-dev-env.zsh
android/gradlew --no-daemon -p android \
  --project-cache-dir "$CLIPSYNC_PROJECT_BUILD_ROOT/gradle-project-cache" \
  :app:assembleDebug --console=plain
```

Expected: BUILD SUCCESSFUL。

- [ ] **Step 5: Commit**

```bash
git add android/app/src/main/java/com/clipsync/app/MainActivity.kt
git commit -m "feat(android): render history entries as rounded cards with clipped thumbnails"
```

---

### Task 5: FocusClipboardActivity 转为 Compose 并接入主题

**Files:**
- Modify: `android/app/src/main/java/com/clipsync/app/FocusClipboardActivity.kt`（整文件重写）

**Interfaces:**
- Consumes: Task 2 的 `ClipSyncTheme`；现有 `ClipboardCapture`、`ClipboardCaptureResult`、`SendResult`、`AppContract` 不变。
- Produces: 同样的对外行为（焦点读取剪贴板 → 发送 → Toast → finish），UI 变为 Compose 状态页。

- [ ] **Step 1: 整文件替换**

`FocusClipboardActivity.kt` 完整替换为：

```kotlin
package com.clipsync.app

import android.content.Context
import android.os.Bundle
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.clipsync.app.ui.theme.ClipSyncTheme

class FocusClipboardActivity : ComponentActivity() {
    private var statusText by mutableStateOf("")
    private var clipboardRead = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        ClipboardSyncService.start(this)
        statusText = getString(R.string.focus_reading)
        setContent {
            ClipSyncTheme {
                Surface(modifier = Modifier.fillMaxSize()) {
                    Box(
                        modifier = Modifier
                            .fillMaxSize()
                            .safeDrawingPadding()
                            .padding(horizontal = 32.dp),
                        contentAlignment = Alignment.Center,
                    ) {
                        Text(
                            text = statusText,
                            style = MaterialTheme.typography.titleMedium,
                            textAlign = TextAlign.Center,
                        )
                    }
                }
            }
        }
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus && !clipboardRead) {
            clipboardRead = true
            getSharedPreferences(AppContract.PREFERENCES, Context.MODE_PRIVATE).edit()
                .putBoolean(AppContract.KEY_WINDOW_FOCUS_READ, true)
                .commit()
            when (val captured = ClipboardCapture.read(this)) {
                is ClipboardCaptureResult.Ready -> send(captured.payload)
                ClipboardCaptureResult.Empty -> finishWith(SendResult.Empty)
                ClipboardCaptureResult.Oversize -> finishWith(SendResult.Oversize)
                ClipboardCaptureResult.Unsupported -> finishWith(SendResult.Unsupported)
            }
        }
    }

    private fun send(payload: ClipboardPayload) {
        statusText = getString(R.string.focus_sending)
        (application as ClipSyncApp).send(payload) { result ->
            runOnUiThread { finishWith(result) }
        }
    }

    private fun finishWith(result: SendResult) {
        getSharedPreferences(AppContract.PREFERENCES, Context.MODE_PRIVATE).edit()
            .putString(AppContract.KEY_LAST_SEND_RESULT, result.preferenceValue())
            .commit()
        val message = when (result) {
            is SendResult.Sent -> R.string.send_success
            is SendResult.Failed -> R.string.send_failed
            SendResult.Empty -> R.string.send_empty
            SendResult.Oversize -> R.string.send_oversize
            SendResult.ServiceUnavailable -> R.string.send_service_unavailable
            SendResult.Unsupported -> R.string.send_unsupported
        }
        Toast.makeText(this, message, Toast.LENGTH_LONG).show()
        finish()
    }
}
```

注意：与旧实现相比，只有「Activity 基类从 `android.app.Activity` 换成 `ComponentActivity`、TextView 换 Compose」两点变化；偏好写入、读取时机、Toast、finish 逻辑逐行保留。`ClipboardCapture` / `ClipboardCaptureResult` / `SendResult` / `AppContract` 与本类同包，无需 import。

- [ ] **Step 2: 编译验证**

```bash
source scripts/external-dev-env.zsh
android/gradlew --no-daemon -p android \
  --project-cache-dir "$CLIPSYNC_PROJECT_BUILD_ROOT/gradle-project-cache" \
  :app:assembleDebug --console=plain
```

Expected: BUILD SUCCESSFUL。

- [ ] **Step 3: Commit**

```bash
git add android/app/src/main/java/com/clipsync/app/FocusClipboardActivity.kt
git commit -m "feat(android): rewrite FocusClipboardActivity in Compose with ClipSyncTheme"
```

---

### Task 6: 全量验证（lint/单测/双 API 模拟器流程 + API 35 截图人工核对）

**Files:**
- 无代码改动；产物截图落在 `$CLIPSYNC_TEST_OUTPUT_ROOT/`（外部卷）。

**Interfaces:**
- Consumes: Task 1–5 的全部改动。
- Produces: 通过/不通过结论 + 6 张核对截图（API 35 配对页、历史页、后台页 × 浅色/深色）。

- [ ] **Step 1: 跑既有双 API 验证脚本（含 lint、单测、assemble、API 29/34 模拟器全流程）**

```bash
source scripts/external-dev-env.zsh
bash scripts/test-android-core.sh
```

Expected: 结尾输出 `PASS: Android T14 dual-API production validation complete`。此脚本同时会把 API 29/34 的主界面截图存到 `$CLIPSYNC_TEST_OUTPUT_ROOT/task-14-android/api{29,34}-status.png`。

- [ ] **Step 2: 启动 API 35 模拟器并安装**

```bash
source scripts/external-dev-env.zsh
ADB="$ANDROID_HOME/platform-tools/adb"
"$ANDROID_HOME/emulator/emulator" -avd clipsync-spike-api35 -port 5584 \
  -no-snapshot -no-audio -no-boot-anim -gpu swiftshader_indirect &
"$ADB" -s emulator-5584 wait-for-device
while [[ $("$ADB" -s emulator-5584 shell getprop sys.boot_completed | tr -d '\r') != "1" ]]; do sleep 2; done
"$ADB" -s emulator-5584 install -r -t "$CLIPSYNC_ANDROID_BUILD_ROOT/app/outputs/apk/debug/app-debug.apk"
```

Expected: install 输出 `Success`。

- [ ] **Step 3: 浅色模式三张截图**

```bash
"$ADB" -s emulator-5584 shell cmd uimode night no
"$ADB" -s emulator-5584 shell am start -W -n com.clipsync.app/.MainActivity
sleep 2
SHOT_DIR="$CLIPSYNC_TEST_OUTPUT_ROOT/expressive-redesign"
mkdir -p "$SHOT_DIR"
"$ADB" -s emulator-5584 exec-out screencap -p > "$SHOT_DIR/api35-light-pairing.png"
"$ADB" -s emulator-5584 shell input tap 540 460   # 「历史」Tab（Pixel 2 1080 宽屏的 TabRow 中部，如点偏用 uiautomator dump 校正坐标）
sleep 1
"$ADB" -s emulator-5584 exec-out screencap -p > "$SHOT_DIR/api35-light-history.png"
"$ADB" -s emulator-5584 shell input tap 900 460   # 「后台」Tab
sleep 1
"$ADB" -s emulator-5584 exec-out screencap -p > "$SHOT_DIR/api35-light-power.png"
```

- [ ] **Step 4: 深色模式三张截图**

同上，先 `"$ADB" -s emulator-5584 shell cmd uimode night yes`，文件名 `api35-dark-*.png`。

- [ ] **Step 5: 人工核对截图并收尾**

逐张查看 `$SHOT_DIR/*.png`，核对清单：
- 历史项是圆角卡片、无分隔线；缩略图有圆角
- 内容不被状态栏/导航栏遮挡；底部按钮不压手势条
- 浅色/深色配色正常（动态取色生效：非默认紫色即成功，模拟器壁纸为系统默认花色）
- 标题/Tab/按钮字体渲染无错位

核对后关模拟器：`"$ADB" -s emulator-5584 emu kill`。

Expected: 六项全部通过；任一项不通过回到对应 Task 修复后重跑本 Task。

- [ ] **Step 6: Commit（仅当验证中发现问题并修复后）**

```bash
git add -A android
git commit -m "fix(android): address issues found in expressive redesign validation"
```

---

## Self-Review 记录

- **Spec coverage**：视觉基准(Expressive)→Task 2；形状+动效→Task 2（默认 motionScheme/shapes）；动态取色+回落→Task 2；深浅色→Task 2；targetSdk 35+edge-to-edge→Task 1+3+5；全部界面→Task 3(主页+扫码)+4(历史)+5(Focus)；圆角默认表→Task 2/4（组件不覆盖 shape）；验证→Task 6。无遗漏。
- **Placeholder scan**：无 TBD/TODO；所有代码步骤含完整代码；Task 1 Step 3 的两条失败处置均给出具体改法与命令。
- **Type consistency**：`ClipSyncTheme(content: @Composable () -> Unit)` 在 Task 2 定义、Task 3/5 调用一致；`HistoryRow(item: CoreHistoryItem, app: ClipSyncApp)`、`ClipSyncScreen(...)` 签名保持现状，Task 3/4 的改法与之兼容。
