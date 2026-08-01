package com.clipsync.app

import android.content.Context
import android.content.SharedPreferences
import android.os.SystemClock
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.uiautomator.By
import androidx.test.uiautomator.UiDevice
import androidx.test.uiautomator.UiObject2
import androidx.test.uiautomator.Until
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue

internal object PlatformTestSupport {
    private const val COMPONENT = "com.clipsync.app/.ClipSyncTileService"
    private const val ACTIVITY = "com.clipsync.app/.FocusClipboardActivity"
    private const val SERVICE = "com.clipsync.app/.ClipboardSyncService"
    private const val SYSTEMUI_PACKAGE = "com.android.systemui"
    private const val TIMEOUT_MILLIS = 15_000L

    val instrumentation = InstrumentationRegistry.getInstrumentation()
    val context: Context = instrumentation.targetContext
    val device: UiDevice = UiDevice.getInstance(instrumentation)
    val preferences: SharedPreferences =
        context.getSharedPreferences(SpikeContract.PREFERENCES, Context.MODE_PRIVATE)

    fun resetFixture() {
        shell("am stopservice -n $SERVICE")
        preferences.edit().clear().commit()
        shell("cmd statusbar collapse")
        device.pressHome()
        device.waitForIdle()
    }

    fun stopFixture() {
        shell("am stopservice -n $SERVICE")
        shell("cmd statusbar collapse")
        device.pressHome()
        device.waitForIdle()
    }

    fun startForegroundService(action: String, clipboardText: String? = null) {
        val textArgument = clipboardText?.let { " --es ${SpikeContract.EXTRA_CLIPBOARD_TEXT} $it" }.orEmpty()
        val output = shell("am start-foreground-service -n $SERVICE -a $action$textArgument")
        assertTrue("foreground service did not start: $output", output.contains("Starting service"))
    }

    fun launchFocusActivity() {
        val output = shell("am start -W -n $ACTIVITY")
        assertTrue("focus activity did not launch: $output", output.contains("Status: ok"))
    }

    fun provisionTile() {
        shell("cmd statusbar remove-tile $COMPONENT")
        val output = shell("cmd statusbar add-tile $COMPONENT")
        assertTrue("statusbar add-tile failed: $output", !output.contains("Error", ignoreCase = true))

        // On a cold SystemUI the freshly added custom tile may not render on
        // the first expand; cycle the panel until it shows up.
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
        // A missing tile only means this attempt fails; the caller retries
        // and ultimately times out if the tile never becomes clickable.
        val tile = findTile() ?: return
        tile.click()
    }

    fun awaitStringPreference(key: String, attempts: Int = 1, action: () -> Unit): String {
        repeat(attempts) {
            val latch = CountDownLatch(1)
            val listener = SharedPreferences.OnSharedPreferenceChangeListener { _, changedKey ->
                if (changedKey == key) {
                    latch.countDown()
                }
            }
            preferences.registerOnSharedPreferenceChangeListener(listener)
            try {
                action()
                latch.await(TIMEOUT_MILLIS, TimeUnit.MILLISECONDS)
                // SharedPreferences does not re-notify for unchanged values,
                // so also accept a value that is already present.
                preferences.getString(key, null)?.let { return it }
            } finally {
                preferences.unregisterOnSharedPreferenceChangeListener(listener)
            }
        }
        throw AssertionError("timed out waiting for preference $key")
    }

    fun assertFocusRead(expectedClipboardText: String) {
        assertEquals(expectedClipboardText, preferences.getString(SpikeContract.KEY_FOCUS_READ, null))
        assertEquals("window_focus", preferences.getString(SpikeContract.KEY_READ_TRIGGER, null))
        assertEquals("true", preferences.getString(SpikeContract.KEY_HAS_WINDOW_FOCUS, null))
    }

    fun shell(command: String): String = device.executeShellCommand(command).trim()

    private fun openQuickSettingsPanel() {
        // UiDevice.openQuickSettings() relies on gestures that are unreliable
        // on a headless emulator; expand through the statusbar shell instead
        // and verify the tile grid really appeared before returning.
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
        // The API 34 Quick Settings pager can reopen on a later page; swipe
        // back to the first page before scanning forward for the tile. Scope
        // every lookup to SystemUI so the launcher icon cannot satisfy it.
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
            device.wait(Until.findObject(By.pkg(SYSTEMUI_PACKAGE).descContains(SpikeContract.TILE_LABEL)), 2_000)
                ?.let { return it }
            device.findObject(By.pkg(SYSTEMUI_PACKAGE).text(SpikeContract.TILE_LABEL))?.let { return it }
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
