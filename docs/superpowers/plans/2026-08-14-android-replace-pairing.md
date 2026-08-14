# Android Replace Pairing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Allow a paired Android client to confirm replacement of its current pairing, reset through the existing Core API, preserve local history, and automatically enter the QR scanning flow for a new Mac.

**Architecture:** Extend the existing Android `CoreGateway` with `resetPairing()`, keep replacement progress and scanner intent in `ClipSyncUiState`, and execute reset work on the existing single-thread Core executor. Compose renders confirmation, progress, permission, and scanning from application-owned state; Rust Core, UniFFI bindings, protocol, and relay remain unchanged.

**Tech Stack:** Kotlin 2.2, Android `Application`, `StateFlow`, Jetpack Compose Material 3, CameraX/ML Kit QR scanning, UniFFI `CoreHandle`, AndroidX instrumentation, UI Automator, Gradle managed devices.

---

## Execution Preconditions

- Design source: `docs/superpowers/specs/2026-08-14-android-replace-pairing-design.md`.
- Start implementation from the commit containing this plan or a later descendant.
- The main workspace contains unrelated user changes. At execution time, use `superpowers:using-git-worktrees` to create an isolated worktree rather than cleaning or reverting the current workspace.
- Do not edit generated `android/app/src/main/java/uniffi/clipboard_core/clipboard_core.kt`; `CoreHandle.resetPairing()` is already generated.
- Before every Gradle build, test, emulator, or packaging command, run the external-storage preflight shown in this plan. Stop immediately if any check fails.

External-storage preflight:

```zsh
source scripts/external-dev-env.zsh
[[ "$EXTERNAL_DEV_READY" == 1 ]]
[[ "$CLIPSYNC_EXTERNAL_READY" == 1 ]]
[[ "$CLIPSYNC_EXTERNAL_LINKS_READY" == 1 ]]
[[ "$(diskutil info -plist "$EXTERNAL_DEV_VOLUME" | plutil -extract FilesystemType raw -o - -)" == apfs ]]
[[ -w "$EXTERNAL_DEV_VOLUME" ]]
DEV_PROJECT_ROOT="$(external_dev_project_root "$PWD")"
mkdir -p "$DEV_PROJECT_ROOT"
```

## File Map

- Modify `android/app/src/main/java/com/clipsync/app/CoreGateway.kt`: expose `resetPairing()` and delegate it to `CoreHandle`.
- Modify `android/app/src/main/java/com/clipsync/app/ClipSyncApp.kt`: own replacement progress, scanner request state, success/failure transitions, and duplicate-request protection.
- Modify `android/app/src/main/java/com/clipsync/app/MainActivity.kt`: render the replacement confirmation dialog, progress state, permission request, and state-driven scanner.
- Modify `android/app/src/androidTest/java/com/clipsync/app/PlatformTestSupport.kt`: configure paired/failing/blocking fake gateways and record reset calls.
- Modify `android/app/src/androidTest/java/com/clipsync/app/PairingHistoryTest.kt`: cover replacement success, history preservation, failure snapshot, duplicate suppression, scanner state, and UI confirmation.

No new production file is needed. The existing files already own the relevant responsibilities.

### Task 1: Add the Successful Replacement State Flow

**Files:**
- Modify: `android/app/src/androidTest/java/com/clipsync/app/PairingHistoryTest.kt:14-109`
- Modify: `android/app/src/androidTest/java/com/clipsync/app/PlatformTestSupport.kt:24-82,234-313`
- Modify: `android/app/src/main/java/com/clipsync/app/CoreGateway.kt:8-72`
- Modify: `android/app/src/main/java/com/clipsync/app/ClipSyncApp.kt:21-195`

- [ ] **Step 1: Write failing instrumentation tests for successful replacement and history preservation**

Add `assertFalse` to the JUnit imports in `PairingHistoryTest.kt`, then add these tests before `deferredHistoryIsPromotedOnlyAfterApplyingToClipboard`:

```kotlin
@Test
fun replacePairingResetsCurrentPairingAndRequestsScanner() {
    PlatformTestSupport.installPairedGateway()
    PlatformTestSupport.app.startCoreAfterForeground()
    awaitState { it.pairing is PairingUiState.Paired }

    PlatformTestSupport.app.replacePairing()

    val state = awaitState {
        it.pairing == PairingUiState.Unpaired &&
            !it.pairingResetInProgress &&
            it.scannerRequested
    }
    assertEquals(
        1,
        PlatformTestSupport.preferences.getInt(
            PlatformTestSupport.KEY_TEST_PAIR_RESET_COUNT,
            0,
        ),
    )
    assertEquals("未配对", state.coreStatus)
    assertFalse(state.historyLoading)
}

@Test
fun replacePairingPreservesLocalHistory() {
    PlatformTestSupport.installPairedGateway(withDeferredHistory = true)
    PlatformTestSupport.app.startCoreAfterForeground()
    val before = awaitState {
        it.pairing is PairingUiState.Paired && it.history.isNotEmpty()
    }.history

    PlatformTestSupport.app.replacePairing()

    val after = awaitState {
        it.pairing == PairingUiState.Unpaired && it.scannerRequested
    }.history
    assertEquals(before, after)
}
```

- [ ] **Step 2: Run Android test compilation and verify RED**

Run:

```zsh
source scripts/external-dev-env.zsh
[[ "$EXTERNAL_DEV_READY" == 1 ]]
[[ "$CLIPSYNC_EXTERNAL_READY" == 1 ]]
[[ "$CLIPSYNC_EXTERNAL_LINKS_READY" == 1 ]]
[[ "$(diskutil info -plist "$EXTERNAL_DEV_VOLUME" | plutil -extract FilesystemType raw -o - -)" == apfs ]]
[[ -w "$EXTERNAL_DEV_VOLUME" ]]
DEV_PROJECT_ROOT="$(external_dev_project_root "$PWD")"
mkdir -p "$DEV_PROJECT_ROOT"
android/gradlew --no-daemon -p android \
  --project-cache-dir "$CLIPSYNC_PROJECT_BUILD_ROOT/gradle-project-cache" \
  assembleDebugAndroidTest --console=plain
```

Expected: compilation fails because `installPairedGateway`, `replacePairing`, `pairingResetInProgress`, `scannerRequested`, and `KEY_TEST_PAIR_RESET_COUNT` do not exist.

- [ ] **Step 3: Extend the fake Gateway for a successful reset**

Add this constant beside the other test preference keys in `PlatformTestSupport`:

```kotlin
const val KEY_TEST_PAIR_RESET_COUNT = "test_pair_reset_count"
```

Add this installer below `installUnpairedGateway`:

```kotlin
fun installPairedGateway(withDeferredHistory: Boolean = false) {
    app.resetCoreGatewayForTest()
    preferences.edit().clear().commit()
    app.installCoreGatewayForTest(
        RecordingCoreGateway(
            preferences = preferences,
            initiallyPaired = true,
            withDeferredHistory = withDeferredHistory,
        ),
    )
}
```

Change `RecordingCoreGateway.loadPairingAndStart()` to return the fake's actual pairing state after recording startup:

```kotlin
override fun loadPairingAndStart(): Boolean {
    val attempt = preferences.getInt(PlatformTestSupport.KEY_TEST_GATEWAY_START_ATTEMPTS, 0) + 1
    preferences.edit()
        .putInt(PlatformTestSupport.KEY_TEST_GATEWAY_START_ATTEMPTS, attempt)
        .commit()
    if (attempt <= failuresBeforeStart) error("simulated transient startup failure")
    preferences.edit().putBoolean(PlatformTestSupport.KEY_TEST_GATEWAY_STARTED, true).commit()
    return pairing is PairingUiState.Paired
}
```

Add this fake Gateway method below `cancelPairing()`:

```kotlin
override fun resetPairing() {
    val count = preferences.getInt(PlatformTestSupport.KEY_TEST_PAIR_RESET_COUNT, 0) + 1
    preferences.edit()
        .putInt(PlatformTestSupport.KEY_TEST_PAIR_RESET_COUNT, count)
        .commit()
    pairing = PairingUiState.Unpaired
}
```

- [ ] **Step 4: Add the Gateway contract and native delegation**

Add the method to `CoreGateway` after `cancelPairing()`:

```kotlin
fun resetPairing()
```

Add the native implementation after `cancelPairing()`:

```kotlin
override fun resetPairing() = handle.resetPairing()
```

- [ ] **Step 5: Add UI state fields and the minimal successful replacement implementation**

Extend `ClipSyncUiState` immediately after `pairing`:

```kotlin
val pairingResetInProgress: Boolean = false,
val scannerRequested: Boolean = false,
```

Add this method in `ClipSyncApp` after `cancelPairing()`:

```kotlin
fun replacePairing() {
    val current = mutableUiState.value
    if (current.pairing !is PairingUiState.Paired) return
    mutableUiState.value = current.copy(
        pairingResetInProgress = true,
        scannerRequested = false,
        coreStatus = "正在更换配对",
        lastError = null,
    )
    executor.execute {
        val gateway = synchronized(coreLock) { coreSlot.gateway }
        if (gateway == null) {
            mutableUiState.value = mutableUiState.value.copy(pairingResetInProgress = false)
            reportError("更换配对失败", IllegalStateException("同步服务尚未就绪"))
            return@execute
        }
        gateway.resetPairing()
        mutableUiState.value = mutableUiState.value.copy(
            pairing = PairingUiState.Unpaired,
            pairingResetInProgress = false,
            scannerRequested = true,
            coreStatus = "未配对",
            lastError = null,
        )
    }
}
```

Do not alter `history` in any copy operation.

Update the test-only reset helper so every instrumentation test starts from clean ephemeral UI state while preserving the current notification permission snapshot:

```kotlin
@VisibleForTesting
fun resetCoreGatewayForTest() {
    synchronized(coreLock) {
        coreSlot = CoreSlot(generation = coreSlot.generation + 1)
    }
    mutableUiState.value = ClipSyncUiState(
        notificationState = mutableUiState.value.notificationState,
    )
}
```

- [ ] **Step 6: Run the targeted instrumentation class and verify GREEN**

Run:

```zsh
source scripts/external-dev-env.zsh
[[ "$EXTERNAL_DEV_READY" == 1 ]]
[[ "$CLIPSYNC_EXTERNAL_READY" == 1 ]]
[[ "$CLIPSYNC_EXTERNAL_LINKS_READY" == 1 ]]
[[ "$(diskutil info -plist "$EXTERNAL_DEV_VOLUME" | plutil -extract FilesystemType raw -o - -)" == apfs ]]
[[ -w "$EXTERNAL_DEV_VOLUME" ]]
DEV_PROJECT_ROOT="$(external_dev_project_root "$PWD")"
mkdir -p "$DEV_PROJECT_ROOT"
android/gradlew --no-daemon -p android \
  --project-cache-dir "$CLIPSYNC_PROJECT_BUILD_ROOT/gradle-project-cache" \
  api34DebugAndroidTest \
  -Pandroid.testInstrumentationRunnerArguments.class=com.clipsync.app.PairingHistoryTest \
  --console=plain
```

Expected: `PairingHistoryTest` passes on the API 34 managed device.

- [ ] **Step 7: Commit the successful state flow**

```zsh
git add \
  android/app/src/main/java/com/clipsync/app/CoreGateway.kt \
  android/app/src/main/java/com/clipsync/app/ClipSyncApp.kt \
  android/app/src/androidTest/java/com/clipsync/app/PlatformTestSupport.kt \
  android/app/src/androidTest/java/com/clipsync/app/PairingHistoryTest.kt
git commit -m "feat(android): add pairing replacement state flow"
```

### Task 2: Handle Reset Failure and Duplicate Requests

**Files:**
- Modify: `android/app/src/androidTest/java/com/clipsync/app/PairingHistoryTest.kt`
- Modify: `android/app/src/androidTest/java/com/clipsync/app/PlatformTestSupport.kt`
- Modify: `android/app/src/main/java/com/clipsync/app/ClipSyncApp.kt`

- [ ] **Step 1: Write failing tests for actual failure snapshots and duplicate suppression**

Add these tests to `PairingHistoryTest`:

```kotlin
@Test
fun replacePairingFailureUsesActualCoreSnapshot() {
    PlatformTestSupport.installPairedGateway(resetFailureMessage = "disk write failed")
    PlatformTestSupport.app.startCoreAfterForeground()
    awaitState { it.pairing is PairingUiState.Paired }

    PlatformTestSupport.app.replacePairing()

    val state = awaitState { it.lastError?.contains("disk write failed") == true }
    assertEquals(PairingUiState.Unpaired, state.pairing)
    assertFalse(state.pairingResetInProgress)
    assertFalse(state.scannerRequested)
}

@Test
fun duplicateReplacePairingRequestRunsOneReset() {
    PlatformTestSupport.installPairedGateway(blockPairingReset = true)
    PlatformTestSupport.app.startCoreAfterForeground()
    awaitState { it.pairing is PairingUiState.Paired }

    PlatformTestSupport.app.replacePairing()
    assertTrue(
        PlatformTestSupport.awaitIntPreferenceAtLeast(
            PlatformTestSupport.KEY_TEST_PAIR_RESET_COUNT,
            1,
        ),
    )
    PlatformTestSupport.app.replacePairing()
    PlatformTestSupport.preferences.edit()
        .putBoolean(PlatformTestSupport.KEY_TEST_ALLOW_PAIR_RESET, true)
        .commit()

    awaitState { !it.pairingResetInProgress }
    SystemClock.sleep(200)
    assertEquals(
        1,
        PlatformTestSupport.preferences.getInt(
            PlatformTestSupport.KEY_TEST_PAIR_RESET_COUNT,
            0,
        ),
    )
}
```

- [ ] **Step 2: Run the targeted instrumentation class and verify RED**

Run:

```zsh
source scripts/external-dev-env.zsh
[[ "$EXTERNAL_DEV_READY" == 1 ]]
[[ "$CLIPSYNC_EXTERNAL_READY" == 1 ]]
[[ "$CLIPSYNC_EXTERNAL_LINKS_READY" == 1 ]]
[[ "$(diskutil info -plist "$EXTERNAL_DEV_VOLUME" | plutil -extract FilesystemType raw -o - -)" == apfs ]]
[[ -w "$EXTERNAL_DEV_VOLUME" ]]
DEV_PROJECT_ROOT="$(external_dev_project_root "$PWD")"
mkdir -p "$DEV_PROJECT_ROOT"
android/gradlew --no-daemon -p android \
  --project-cache-dir "$CLIPSYNC_PROJECT_BUILD_ROOT/gradle-project-cache" \
  api34DebugAndroidTest \
  -Pandroid.testInstrumentationRunnerArguments.class=com.clipsync.app.PairingHistoryTest \
  --console=plain
```

Expected: test compilation fails because the fake reset controls do not exist, or the tests fail because reset exceptions escape and duplicate requests are queued.

- [ ] **Step 3: Add deterministic failure and blocking controls to the fake Gateway**

Add this preference key:

```kotlin
const val KEY_TEST_ALLOW_PAIR_RESET = "test_allow_pair_reset"
```

Replace `installPairedGateway` with:

```kotlin
fun installPairedGateway(
    withDeferredHistory: Boolean = false,
    resetFailureMessage: String? = null,
    blockPairingReset: Boolean = false,
) {
    app.resetCoreGatewayForTest()
    preferences.edit().clear().commit()
    app.installCoreGatewayForTest(
        RecordingCoreGateway(
            preferences = preferences,
            initiallyPaired = true,
            withDeferredHistory = withDeferredHistory,
            resetFailureMessage = resetFailureMessage,
            blockPairingReset = blockPairingReset,
        ),
    )
}
```

Extend the `RecordingCoreGateway` constructor:

```kotlin
private class RecordingCoreGateway(
    private val preferences: SharedPreferences,
    private val failuresBeforeStart: Int = 0,
    initiallyPaired: Boolean = true,
    withDeferredHistory: Boolean = false,
    private val resetFailureMessage: String? = null,
    private val blockPairingReset: Boolean = false,
) : CoreGateway {
```

Replace its `resetPairing()` with:

```kotlin
override fun resetPairing() {
    val count = preferences.getInt(PlatformTestSupport.KEY_TEST_PAIR_RESET_COUNT, 0) + 1
    preferences.edit()
        .putInt(PlatformTestSupport.KEY_TEST_PAIR_RESET_COUNT, count)
        .commit()
    if (blockPairingReset) {
        val deadline = SystemClock.uptimeMillis() + 10_000
        while (
            !preferences.getBoolean(PlatformTestSupport.KEY_TEST_ALLOW_PAIR_RESET, false) &&
            SystemClock.uptimeMillis() < deadline
        ) {
            SystemClock.sleep(10)
        }
    }
    pairing = PairingUiState.Unpaired
    resetFailureMessage?.let(::error)
}
```

This intentionally moves the fake snapshot to `Unpaired` before throwing, matching the Core contract that installs a fresh empty dispatcher even when reset reports an error.

- [ ] **Step 4: Add duplicate protection and failure snapshot handling**

Replace `replacePairing()` with:

```kotlin
fun replacePairing() {
    val current = mutableUiState.value
    if (current.pairing !is PairingUiState.Paired || current.pairingResetInProgress) return
    mutableUiState.value = current.copy(
        pairingResetInProgress = true,
        scannerRequested = false,
        coreStatus = "正在更换配对",
        lastError = null,
    )
    executor.execute {
        val gateway = synchronized(coreLock) { coreSlot.gateway }
        if (gateway == null) {
            mutableUiState.value = mutableUiState.value.copy(
                pairingResetInProgress = false,
                scannerRequested = false,
            )
            reportError("更换配对失败", IllegalStateException("同步服务尚未就绪"))
            return@execute
        }
        runCatching { gateway.resetPairing() }
            .onSuccess {
                mutableUiState.value = mutableUiState.value.copy(
                    pairing = PairingUiState.Unpaired,
                    pairingResetInProgress = false,
                    scannerRequested = true,
                    coreStatus = "未配对",
                    lastError = null,
                )
            }
            .onFailure { error ->
                val actualPairing = runCatching { gateway.pairingSnapshot() }
                    .getOrDefault(PairingUiState.Unpaired)
                mutableUiState.value = mutableUiState.value.copy(
                    pairing = actualPairing,
                    pairingResetInProgress = false,
                    scannerRequested = false,
                )
                reportError("更换配对失败", error)
            }
    }
}
```

- [ ] **Step 5: Run the targeted instrumentation class and verify GREEN**

Run:

```zsh
source scripts/external-dev-env.zsh
[[ "$EXTERNAL_DEV_READY" == 1 ]]
[[ "$CLIPSYNC_EXTERNAL_READY" == 1 ]]
[[ "$CLIPSYNC_EXTERNAL_LINKS_READY" == 1 ]]
[[ "$(diskutil info -plist "$EXTERNAL_DEV_VOLUME" | plutil -extract FilesystemType raw -o - -)" == apfs ]]
[[ -w "$EXTERNAL_DEV_VOLUME" ]]
DEV_PROJECT_ROOT="$(external_dev_project_root "$PWD")"
mkdir -p "$DEV_PROJECT_ROOT"
android/gradlew --no-daemon -p android \
  --project-cache-dir "$CLIPSYNC_PROJECT_BUILD_ROOT/gradle-project-cache" \
  api34DebugAndroidTest \
  -Pandroid.testInstrumentationRunnerArguments.class=com.clipsync.app.PairingHistoryTest \
  --console=plain
```

Expected: all `PairingHistoryTest` cases pass, including failure and duplicate suppression.

- [ ] **Step 6: Commit failure and concurrency behavior**

```zsh
git add \
  android/app/src/main/java/com/clipsync/app/ClipSyncApp.kt \
  android/app/src/androidTest/java/com/clipsync/app/PlatformTestSupport.kt \
  android/app/src/androidTest/java/com/clipsync/app/PairingHistoryTest.kt
git commit -m "fix(android): guard pairing replacement transitions"
```

### Task 3: Centralize Scanner Request State

**Files:**
- Modify: `android/app/src/androidTest/java/com/clipsync/app/PairingHistoryTest.kt`
- Modify: `android/app/src/main/java/com/clipsync/app/ClipSyncApp.kt`

- [ ] **Step 1: Write failing tests for manual scanner control and claim transition**

Add these tests to `PairingHistoryTest`:

```kotlin
@Test
fun unpairedUserCanRequestAndCancelScanner() {
    PlatformTestSupport.app.startCoreAfterForeground()
    awaitState { it.pairing == PairingUiState.Unpaired }

    PlatformTestSupport.app.requestPairingScan()
    assertTrue(PlatformTestSupport.app.uiState.value.scannerRequested)

    PlatformTestSupport.app.cancelPairingScan()
    assertFalse(PlatformTestSupport.app.uiState.value.scannerRequested)
}

@Test
fun claimingPairingConsumesScannerRequest() {
    PlatformTestSupport.app.startCoreAfterForeground()
    awaitState { it.pairing == PairingUiState.Unpaired }
    PlatformTestSupport.app.requestPairingScan()

    PlatformTestSupport.app.claimPairing(PlatformTestSupport.VALID_TEST_QR)

    val state = awaitState { it.pairing == PairingUiState.SasReady("123456") }
    assertFalse(state.scannerRequested)
}
```

- [ ] **Step 2: Run Android test compilation and verify RED**

Run:

```zsh
source scripts/external-dev-env.zsh
[[ "$EXTERNAL_DEV_READY" == 1 ]]
[[ "$CLIPSYNC_EXTERNAL_READY" == 1 ]]
[[ "$CLIPSYNC_EXTERNAL_LINKS_READY" == 1 ]]
[[ "$(diskutil info -plist "$EXTERNAL_DEV_VOLUME" | plutil -extract FilesystemType raw -o - -)" == apfs ]]
[[ -w "$EXTERNAL_DEV_VOLUME" ]]
DEV_PROJECT_ROOT="$(external_dev_project_root "$PWD")"
mkdir -p "$DEV_PROJECT_ROOT"
android/gradlew --no-daemon -p android \
  --project-cache-dir "$CLIPSYNC_PROJECT_BUILD_ROOT/gradle-project-cache" \
  assembleDebugAndroidTest --console=plain
```

Expected: compilation fails because `requestPairingScan()` and `cancelPairingScan()` do not exist.

- [ ] **Step 3: Implement scanner request operations and consume the request on claim**

Add these methods before `claimPairing()` in `ClipSyncApp`:

```kotlin
fun requestPairingScan() {
    val current = mutableUiState.value
    if (current.pairing != PairingUiState.Unpaired || current.pairingResetInProgress) return
    mutableUiState.value = current.copy(scannerRequested = true, lastError = null)
}

fun cancelPairingScan() {
    mutableUiState.value = mutableUiState.value.copy(scannerRequested = false)
}
```

Update the first state change in `claimPairing()` to consume the request:

```kotlin
mutableUiState.value = mutableUiState.value.copy(
    pairing = PairingUiState.Claiming,
    scannerRequested = false,
    lastError = null,
)
```

Update the success state in `cancelPairing()` so all cancellation paths agree:

```kotlin
mutableUiState.value = mutableUiState.value.copy(
    pairing = PairingUiState.Unpaired,
    scannerRequested = false,
    coreStatus = "未配对",
    lastError = null,
)
```

- [ ] **Step 4: Run the targeted instrumentation class and verify GREEN**

Run:

```zsh
source scripts/external-dev-env.zsh
[[ "$EXTERNAL_DEV_READY" == 1 ]]
[[ "$CLIPSYNC_EXTERNAL_READY" == 1 ]]
[[ "$CLIPSYNC_EXTERNAL_LINKS_READY" == 1 ]]
[[ "$(diskutil info -plist "$EXTERNAL_DEV_VOLUME" | plutil -extract FilesystemType raw -o - -)" == apfs ]]
[[ -w "$EXTERNAL_DEV_VOLUME" ]]
DEV_PROJECT_ROOT="$(external_dev_project_root "$PWD")"
mkdir -p "$DEV_PROJECT_ROOT"
android/gradlew --no-daemon -p android \
  --project-cache-dir "$CLIPSYNC_PROJECT_BUILD_ROOT/gradle-project-cache" \
  api34DebugAndroidTest \
  -Pandroid.testInstrumentationRunnerArguments.class=com.clipsync.app.PairingHistoryTest \
  --console=plain
```

Expected: all scanner-state and existing pairing tests pass.

- [ ] **Step 5: Commit scanner state ownership**

```zsh
git add \
  android/app/src/main/java/com/clipsync/app/ClipSyncApp.kt \
  android/app/src/androidTest/java/com/clipsync/app/PairingHistoryTest.kt
git commit -m "feat(android): centralize pairing scanner state"
```

### Task 4: Add Confirmation, Progress, and Automatic Scanner UI

**Files:**
- Modify: `android/app/src/androidTest/java/com/clipsync/app/PairingHistoryTest.kt`
- Modify: `android/app/src/main/java/com/clipsync/app/MainActivity.kt:174-341`

- [ ] **Step 1: Write a failing UI Automator test for confirmation and automatic scanning**

Add imports to `PairingHistoryTest.kt`:

```kotlin
import android.Manifest
import android.os.Build
import androidx.test.uiautomator.By
import androidx.test.uiautomator.Until
```

Add this test:

```kotlin
@Test
fun replacePairingRequiresConfirmationAndOpensScanner() {
    fun node(text: String) = PlatformTestSupport.device.wait(
        Until.findObject(By.text(text)),
        5_000,
    ) ?: throw AssertionError("missing UI text: $text")

    PlatformTestSupport.installPairedGateway(blockPairingReset = true)
    PlatformTestSupport.app.startCoreAfterForeground()
    awaitState { it.pairing is PairingUiState.Paired }
    PlatformTestSupport.shell("pm grant com.clipsync.app ${Manifest.permission.CAMERA}")
    if (Build.VERSION.SDK_INT >= 33) {
        PlatformTestSupport.shell(
            "pm grant com.clipsync.app ${Manifest.permission.POST_NOTIFICATIONS}",
        )
    }
    PlatformTestSupport.app.markNotificationPermissionAsked()
    PlatformTestSupport.launchMainActivity()

    node("更换配对").click()
    node("更换配对设备？")
    assertEquals(
        0,
        PlatformTestSupport.preferences.getInt(
            PlatformTestSupport.KEY_TEST_PAIR_RESET_COUNT,
            0,
        ),
    )

    node("取消").click()
    assertEquals(
        0,
        PlatformTestSupport.preferences.getInt(
            PlatformTestSupport.KEY_TEST_PAIR_RESET_COUNT,
            0,
        ),
    )

    node("更换配对").click()
    node("更换并扫码").click()
    assertTrue(
        PlatformTestSupport.awaitIntPreferenceAtLeast(
            PlatformTestSupport.KEY_TEST_PAIR_RESET_COUNT,
            1,
        ),
    )
    node("正在移除当前配对")

    PlatformTestSupport.preferences.edit()
        .putBoolean(PlatformTestSupport.KEY_TEST_ALLOW_PAIR_RESET, true)
        .commit()
    awaitState { it.pairing == PairingUiState.Unpaired && it.scannerRequested }
    node("取消扫码")
}
```

- [ ] **Step 2: Run the targeted instrumentation class and verify RED**

Run:

```zsh
source scripts/external-dev-env.zsh
[[ "$EXTERNAL_DEV_READY" == 1 ]]
[[ "$CLIPSYNC_EXTERNAL_READY" == 1 ]]
[[ "$CLIPSYNC_EXTERNAL_LINKS_READY" == 1 ]]
[[ "$(diskutil info -plist "$EXTERNAL_DEV_VOLUME" | plutil -extract FilesystemType raw -o - -)" == apfs ]]
[[ -w "$EXTERNAL_DEV_VOLUME" ]]
DEV_PROJECT_ROOT="$(external_dev_project_root "$PWD")"
mkdir -p "$DEV_PROJECT_ROOT"
android/gradlew --no-daemon -p android \
  --project-cache-dir "$CLIPSYNC_PROJECT_BUILD_ROOT/gradle-project-cache" \
  api34DebugAndroidTest \
  -Pandroid.testInstrumentationRunnerArguments.class=com.clipsync.app.PairingHistoryTest \
  --console=plain
```

Expected: the UI test fails because no “更换配对” action or confirmation dialog exists.

- [ ] **Step 3: Pass full state into the pairing pane**

Replace the pairing tab call in `ClipSyncScreen` with:

```kotlin
AppTab.PAIRING -> PairingPane(
    state = state,
    app = app,
    openCameraSettings = openCameraSettings,
    sendCurrentClipboard = sendCurrentClipboard,
)
```

Change the pairing pane signature and initialize its dialog state:

```kotlin
@Composable
private fun PairingPane(
    state: ClipSyncUiState,
    app: ClipSyncApp,
    openCameraSettings: () -> Unit,
    sendCurrentClipboard: () -> Unit,
) {
    val pairing = state.pairing
    var confirmReplace by remember { mutableStateOf(false) }
```

Delete the old local `scanning` variable.

- [ ] **Step 4: Make permission and scanner rendering state-driven**

Replace the camera permission launcher callback with:

```kotlin
val cameraPermission = androidx.activity.compose.rememberLauncherForActivityResult(
    ActivityResultContracts.RequestPermission(),
) { granted ->
    cameraGranted = granted
    if (!granted) {
        app.cancelPairingScan()
        app.reportUiError(app.getString(R.string.camera_permission_required))
    }
}

LaunchedEffect(state.scannerRequested, cameraGranted, pairing) {
    if (
        state.scannerRequested &&
        !cameraGranted &&
        pairing == PairingUiState.Unpaired
    ) {
        cameraPermission.launch(Manifest.permission.CAMERA)
    }
}
```

Replace the scanner condition and callbacks with:

```kotlin
if (
    state.scannerRequested &&
    cameraGranted &&
    pairing == PairingUiState.Unpaired
) {
    Box(modifier = Modifier.fillMaxSize()) {
        QrScannerView(
            onQrCode = app::claimPairing,
            onError = { message ->
                app.cancelPairingScan()
                app.reportUiError("扫码失败：$message")
            },
        )
        OutlinedButton(
            onClick = app::cancelPairingScan,
            modifier = Modifier
                .align(Alignment.BottomCenter)
                .navigationBarsPadding()
                .padding(24.dp),
        ) {
            Text("取消扫码")
        }
    }
    return
}
```

Replace the unpaired scan button callback with:

```kotlin
Button(onClick = app::requestPairingScan) {
    Text("扫描 Mac 配对码")
}
```

Keep the existing system-settings link when `cameraGranted` is false.

- [ ] **Step 5: Add the paired-state replacement controls and confirmation dialog**

Replace the `PairingUiState.Paired` branch with:

```kotlin
is PairingUiState.Paired -> {
    Text("已配对", style = MaterialTheme.typography.titleLarge)
    Text("房间 ${pairing.roomId.take(12)}")
    if (state.pairingResetInProgress) {
        CircularProgressIndicator()
        Text("正在移除当前配对")
    }
    Button(
        onClick = sendCurrentClipboard,
        enabled = !state.pairingResetInProgress,
    ) {
        Text(stringResource(R.string.send_current_clipboard))
    }
    OutlinedButton(
        onClick = { confirmReplace = true },
        enabled = !state.pairingResetInProgress,
    ) {
        Text("更换配对")
    }
}
```

Add this dialog after the main `Column` in `PairingPane`:

```kotlin
if (confirmReplace) {
    AlertDialog(
        onDismissRequest = { confirmReplace = false },
        title = { Text("更换配对设备？") },
        text = {
            Text("当前配对将被移除，本机历史会保留。随后需要扫描新 Mac 的配对码。")
        },
        confirmButton = {
            TextButton(
                onClick = {
                    confirmReplace = false
                    app.replacePairing()
                },
            ) {
                Text("更换并扫码")
            }
        },
        dismissButton = {
            TextButton(onClick = { confirmReplace = false }) {
                Text("取消")
            }
        },
    )
}
```

- [ ] **Step 6: Run the targeted instrumentation class and verify GREEN**

Run:

```zsh
source scripts/external-dev-env.zsh
[[ "$EXTERNAL_DEV_READY" == 1 ]]
[[ "$CLIPSYNC_EXTERNAL_READY" == 1 ]]
[[ "$CLIPSYNC_EXTERNAL_LINKS_READY" == 1 ]]
[[ "$(diskutil info -plist "$EXTERNAL_DEV_VOLUME" | plutil -extract FilesystemType raw -o - -)" == apfs ]]
[[ -w "$EXTERNAL_DEV_VOLUME" ]]
DEV_PROJECT_ROOT="$(external_dev_project_root "$PWD")"
mkdir -p "$DEV_PROJECT_ROOT"
android/gradlew --no-daemon -p android \
  --project-cache-dir "$CLIPSYNC_PROJECT_BUILD_ROOT/gradle-project-cache" \
  api34DebugAndroidTest \
  -Pandroid.testInstrumentationRunnerArguments.class=com.clipsync.app.PairingHistoryTest \
  --console=plain
```

Expected: the confirmation test finds the dialog, cancellation records zero resets, confirmation exposes progress, and successful reset renders “取消扫码”.

- [ ] **Step 7: Run Android lint and JVM unit tests**

Run:

```zsh
source scripts/external-dev-env.zsh
[[ "$EXTERNAL_DEV_READY" == 1 ]]
[[ "$CLIPSYNC_EXTERNAL_READY" == 1 ]]
[[ "$CLIPSYNC_EXTERNAL_LINKS_READY" == 1 ]]
[[ "$(diskutil info -plist "$EXTERNAL_DEV_VOLUME" | plutil -extract FilesystemType raw -o - -)" == apfs ]]
[[ -w "$EXTERNAL_DEV_VOLUME" ]]
DEV_PROJECT_ROOT="$(external_dev_project_root "$PWD")"
mkdir -p "$DEV_PROJECT_ROOT"
android/gradlew --no-daemon -p android \
  --project-cache-dir "$CLIPSYNC_PROJECT_BUILD_ROOT/gradle-project-cache" \
  lintDebug testDebugUnitTest assembleDebug assembleDebugAndroidTest \
  --console=plain
```

Expected: `BUILD SUCCESSFUL`, with no lint errors or JVM test failures.

- [ ] **Step 8: Commit the Compose interaction**

```zsh
git add \
  android/app/src/main/java/com/clipsync/app/MainActivity.kt \
  android/app/src/androidTest/java/com/clipsync/app/PairingHistoryTest.kt
git commit -m "feat(android): add replace pairing confirmation UI"
```

### Task 5: Full Android Regression and Final Review

**Files:**
- Verify only; modify production files only if a failing test exposes a defect covered by this feature.

- [ ] **Step 1: Run the complete pairing instrumentation class on API 34**

Run:

```zsh
source scripts/external-dev-env.zsh
[[ "$EXTERNAL_DEV_READY" == 1 ]]
[[ "$CLIPSYNC_EXTERNAL_READY" == 1 ]]
[[ "$CLIPSYNC_EXTERNAL_LINKS_READY" == 1 ]]
[[ "$(diskutil info -plist "$EXTERNAL_DEV_VOLUME" | plutil -extract FilesystemType raw -o - -)" == apfs ]]
[[ -w "$EXTERNAL_DEV_VOLUME" ]]
DEV_PROJECT_ROOT="$(external_dev_project_root "$PWD")"
mkdir -p "$DEV_PROJECT_ROOT"
android/gradlew --no-daemon -p android \
  --project-cache-dir "$CLIPSYNC_PROJECT_BUILD_ROOT/gradle-project-cache" \
  api34DebugAndroidTest \
  -Pandroid.testInstrumentationRunnerArguments.class=com.clipsync.app.PairingHistoryTest \
  --console=plain
```

Expected: every `PairingHistoryTest` case passes, including existing QR/SAS/history behavior and the new replacement flow.

- [ ] **Step 2: Run the repository Android API 29/34 regression script**

Run:

```zsh
source scripts/external-dev-env.zsh
[[ "$EXTERNAL_DEV_READY" == 1 ]]
[[ "$CLIPSYNC_EXTERNAL_READY" == 1 ]]
[[ "$CLIPSYNC_EXTERNAL_LINKS_READY" == 1 ]]
[[ "$(diskutil info -plist "$EXTERNAL_DEV_VOLUME" | plutil -extract FilesystemType raw -o - -)" == apfs ]]
[[ -w "$EXTERNAL_DEV_VOLUME" ]]
DEV_PROJECT_ROOT="$(external_dev_project_root "$PWD")"
mkdir -p "$DEV_PROJECT_ROOT"
scripts/test-android-core.sh
```

Expected: build gates pass, API 29 production flow passes, API 34 production and notification flows pass, and the script ends with `PASS: Android T14 dual-API production validation complete`.

- [ ] **Step 3: Inspect only the feature diff**

Run:

```zsh
git diff 4e3b5e9...HEAD -- \
  android/app/src/main/java/com/clipsync/app/CoreGateway.kt \
  android/app/src/main/java/com/clipsync/app/ClipSyncApp.kt \
  android/app/src/main/java/com/clipsync/app/MainActivity.kt \
  android/app/src/androidTest/java/com/clipsync/app/PlatformTestSupport.kt \
  android/app/src/androidTest/java/com/clipsync/app/PairingHistoryTest.kt
git diff --check 4e3b5e9...HEAD
git status --short
```

Expected:

- No protocol, Rust Core, generated UniFFI, server, history-clear, or unrelated UI changes.
- Successful reset preserves `history` and requests scanning.
- Failed reset displays the actual Core snapshot and never auto-opens scanning.
- Duplicate replacement calls execute one reset.
- The scanner is controlled by `scannerRequested`, not a second local boolean.
- Confirmation cancellation performs no Core operation.
- `git diff --check` prints no errors.

- [ ] **Step 4: Commit any test-driven correction, only when one was required**

When Step 1 or Step 2 required a correction within this feature's files, stage only those corrected files and commit them:

```zsh
git add \
  android/app/src/main/java/com/clipsync/app/CoreGateway.kt \
  android/app/src/main/java/com/clipsync/app/ClipSyncApp.kt \
  android/app/src/main/java/com/clipsync/app/MainActivity.kt \
  android/app/src/androidTest/java/com/clipsync/app/PlatformTestSupport.kt \
  android/app/src/androidTest/java/com/clipsync/app/PairingHistoryTest.kt
git commit -m "fix(android): complete replace pairing verification"
```

If no correction was needed, do not create an empty commit.
