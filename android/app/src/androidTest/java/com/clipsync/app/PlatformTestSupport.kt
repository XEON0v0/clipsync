package com.clipsync.app

import android.content.ClipData
import android.content.ClipDescription
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.SharedPreferences
import android.net.Uri
import android.os.SystemClock
import android.view.accessibility.AccessibilityEvent
import androidx.annotation.StringRes
import androidx.core.content.FileProvider
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.uiautomator.By
import androidx.test.uiautomator.UiDevice
import androidx.test.uiautomator.UiObject2
import androidx.test.uiautomator.Until
import java.io.File
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import org.junit.Assert.assertTrue

internal object PlatformTestSupport {
    private const val COMPONENT = "com.clipsync.app/.ClipSyncTileService"
    private const val SYSTEMUI_PACKAGE = "com.android.systemui"
    private const val TIMEOUT_MILLIS = 15_000L
    const val KEY_TEST_GATEWAY_STARTED = "test_gateway_started"
    const val KEY_TEST_GATEWAY_START_ATTEMPTS = "test_gateway_start_attempts"
    const val KEY_TEST_SENT_IMAGE_SIZE = "test_sent_image_size"
    const val KEY_TEST_SENT_TEXT = "test_sent_text"

    val instrumentation = InstrumentationRegistry.getInstrumentation()
    val context: Context = instrumentation.targetContext
    val device: UiDevice = UiDevice.getInstance(instrumentation)
    val preferences: SharedPreferences =
        context.getSharedPreferences(AppContract.PREFERENCES, Context.MODE_PRIVATE)
    val app: ClipSyncApp
        get() = context.applicationContext as ClipSyncApp

    fun resetFixture(installGateway: Boolean = true) {
        context.stopService(android.content.Intent(context, ClipboardSyncService::class.java))
        SystemClock.sleep(250)
        app.resetCoreGatewayForTest()
        preferences.edit().clear().commit()
        if (installGateway) app.installCoreGatewayForTest(RecordingCoreGateway(preferences))
        shell("cmd statusbar collapse")
        device.pressHome()
        device.waitForIdle()
    }

    fun stopFixture() {
        context.stopService(android.content.Intent(context, ClipboardSyncService::class.java))
        shell("cmd statusbar collapse")
        device.pressHome()
        device.waitForIdle()
    }

    fun startForegroundService() {
        ClipboardSyncService.start(context)
        assertTrue("foreground service did not start", awaitBooleanPreference(AppContract.KEY_SERVICE_RUNNING))
    }

    fun installFailOnceGateway() {
        app.installCoreGatewayForTest(RecordingCoreGateway(preferences, failuresBeforeStart = 1))
    }

    fun launchMainActivity() {
        val output = shell("am start -W -n com.clipsync.app/.MainActivity")
        assertTrue("main activity did not launch: $output", output.contains("Status: ok"))
    }

    fun launchFocusActivity() {
        instrumentation.runOnMainSync {
            context.startActivity(
                Intent(context, FocusClipboardActivity::class.java).apply {
                    addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TASK)
                },
            )
        }
    }

    fun launchFocusActivityAndAwaitToast(@StringRes messageId: Int): String {
        val expected = context.getString(messageId)
        instrumentation.uiAutomation.executeAndWaitForEvent(
            { launchFocusActivity() },
            { event ->
                event.eventType == AccessibilityEvent.TYPE_NOTIFICATION_STATE_CHANGED &&
                    event.text.any { it.toString() == expected }
            },
            TIMEOUT_MILLIS,
        )
        return preferences.getString(AppContract.KEY_LAST_SEND_RESULT, null)
            ?: throw AssertionError("Toast appeared before send result was persisted")
    }

    fun setClipboardText(text: String) {
        context.getSystemService(ClipboardManager::class.java)
            .setPrimaryClip(ClipData.newPlainText("ClipSync test", text))
    }

    fun setClipboardImage(uri: Uri) {
        context.getSystemService(ClipboardManager::class.java).setPrimaryClip(
            ClipData(
                ClipDescription("ClipSync test image", arrayOf("image/png")),
                ClipData.Item(uri),
            ),
        )
    }

    fun fileProviderUri(file: File): Uri = FileProvider.getUriForFile(
        context,
        context.packageName + AppContract.FILE_PROVIDER_AUTHORITY_SUFFIX,
        file,
    )

    fun provisionTile() {
        shell("cmd statusbar remove-tile $COMPONENT")
        val output = shell("cmd statusbar add-tile $COMPONENT")
        assertTrue("statusbar add-tile failed: $output", !output.contains("Error", ignoreCase = true))

        var tile: UiObject2? = null
        for (attempt in 1..3) {
            openQuickSettingsPanel()
            tile = findTile()
            if (tile != null) break
            shell("cmd statusbar collapse")
            device.waitForIdle()
        }
        assertTrue("tile was not provisioned in the real Quick Settings panel", tile != null)
        val settings = shell("settings get secure sysui_qs_tiles")
        assertTrue("tile missing from sysui_qs_tiles: $settings", settings.contains("ClipSyncTileService"))
        shell("cmd statusbar collapse")
    }

    fun clickProvisionedTile() {
        openQuickSettingsPanel()
        findTile()?.click()
    }

    fun awaitStringPreference(key: String, attempts: Int = 1, action: () -> Unit): String {
        repeat(attempts) {
            val latch = CountDownLatch(1)
            val listener = SharedPreferences.OnSharedPreferenceChangeListener { _, changedKey ->
                if (changedKey == key) latch.countDown()
            }
            preferences.registerOnSharedPreferenceChangeListener(listener)
            try {
                action()
                latch.await(TIMEOUT_MILLIS, TimeUnit.MILLISECONDS)
                preferences.getString(key, null)?.let { return it }
            } finally {
                preferences.unregisterOnSharedPreferenceChangeListener(listener)
            }
        }
        throw AssertionError("timed out waiting for preference $key")
    }

    fun awaitBooleanPreference(key: String): Boolean {
        val deadline = SystemClock.uptimeMillis() + TIMEOUT_MILLIS
        while (SystemClock.uptimeMillis() < deadline) {
            if (preferences.getBoolean(key, false)) return true
            SystemClock.sleep(100)
        }
        return false
    }

    fun awaitIntPreferenceAtLeast(key: String, expected: Int): Boolean {
        val deadline = SystemClock.uptimeMillis() + TIMEOUT_MILLIS
        while (SystemClock.uptimeMillis() < deadline) {
            if (preferences.getInt(key, 0) >= expected) return true
            SystemClock.sleep(100)
        }
        return false
    }

    fun shell(command: String): String = device.executeShellCommand(command).trim()

    private fun openQuickSettingsPanel() {
        repeat(5) {
            shell("cmd statusbar expand-settings")
            device.waitForIdle()
            SystemClock.sleep(500)
            device.wait(Until.findObject(By.pkg(SYSTEMUI_PACKAGE).res("$SYSTEMUI_PACKAGE:id/tile_page")), 1_500)
                ?.let { return }
        }
        throw AssertionError("Quick Settings panel did not open")
    }

    private fun findTile(): UiObject2? {
        repeat(4) {
            device.swipe(
                device.displayWidth / 4,
                device.displayHeight / 2,
                device.displayWidth * 3 / 4,
                device.displayHeight / 2,
                30,
            )
            SystemClock.sleep(250)
        }
        repeat(6) {
            device.wait(Until.findObject(By.pkg(SYSTEMUI_PACKAGE).descContains("ClipSync")), 2_000)
                ?.let { return it }
            device.findObject(By.pkg(SYSTEMUI_PACKAGE).text("ClipSync"))?.let { return it }
            device.swipe(
                device.displayWidth * 3 / 4,
                device.displayHeight / 2,
                device.displayWidth / 4,
                device.displayHeight / 2,
                30,
            )
            SystemClock.sleep(250)
        }
        return null
    }
}

private class RecordingCoreGateway(
    private val preferences: SharedPreferences,
    private val failuresBeforeStart: Int = 0,
) : CoreGateway {
    override fun loadPairingAndStart(): Boolean {
        val attempt = preferences.getInt(PlatformTestSupport.KEY_TEST_GATEWAY_START_ATTEMPTS, 0) + 1
        preferences.edit()
            .putInt(PlatformTestSupport.KEY_TEST_GATEWAY_START_ATTEMPTS, attempt)
            .commit()
        if (attempt <= failuresBeforeStart) error("simulated transient startup failure")
        preferences.edit().putBoolean(PlatformTestSupport.KEY_TEST_GATEWAY_STARTED, true).commit()
        return true
    }

    override fun sendImage(bytes: ByteArray): ULong {
        preferences.edit()
            .putInt(PlatformTestSupport.KEY_TEST_SENT_IMAGE_SIZE, bytes.size)
            .commit()
        return 42UL
    }

    override fun sendText(text: String): ULong {
        preferences.edit().putString(PlatformTestSupport.KEY_TEST_SENT_TEXT, text).commit()
        return 42UL
    }

    override fun shutdown() = Unit
}
