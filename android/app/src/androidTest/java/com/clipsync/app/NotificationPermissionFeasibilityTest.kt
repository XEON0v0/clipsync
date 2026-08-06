package com.clipsync.app

import android.Manifest
import android.app.NotificationManager
import android.content.pm.PackageManager
import android.os.Build
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.uiautomator.By
import androidx.test.uiautomator.Until
import java.util.UUID
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import uniffi.clipboard_core.FfiClipContent
import uniffi.clipboard_core.FfiClipItem
import uniffi.clipboard_core.MailboxDisposition

@RunWith(AndroidJUnit4::class)
class NotificationPermissionFeasibilityTest {
    @Before
    fun givenCleanApi34Fixture() {
        org.junit.Assume.assumeTrue("notification permission fixture requires API 34", Build.VERSION.SDK_INT == 34)
        PlatformTestSupport.resetFixture()
    }

    @After
    fun cleanupApi34Fixture() {
        PlatformTestSupport.stopFixture()
    }

    @Test
    fun firstLaunchPermissionBranchKeepsServiceRunningAndSurfacesStatus() {
        // Harness scripts pass the branch explicitly; bare managed-device runs get no
        // arguments and always boot a fresh emulator in the denied state, so they take
        // the restricted branch.
        val expected = InstrumentationRegistry.getArguments().getString("expectedNotificationState")
            ?: NotificationState.RESTRICTED.persistedValue
        assertEquals(
            PackageManager.PERMISSION_DENIED,
            PlatformTestSupport.context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS),
        )

        PlatformTestSupport.launchMainActivity()
        assertTrue(
            "first launch did not request notification permission",
            respondToPermissionDialog(grant = expected == NotificationState.VISIBLE.persistedValue),
        )

        val label = if (expected == NotificationState.VISIBLE.persistedValue) "通知正常" else "通知受限"
        assertNotNull(
            "status surface did not report $label",
            PlatformTestSupport.device.wait(Until.findObject(By.text("通知状态：$label")), 10_000),
        )
        assertTrue(PlatformTestSupport.awaitBooleanPreference(AppContract.KEY_SERVICE_RUNNING))
        assertEquals(expected, AppContract.notificationState(PlatformTestSupport.context).persistedValue)

        val manager = PlatformTestSupport.context.getSystemService(NotificationManager::class.java)
        manager.cancel(AppContract.MAILBOX_NOTIFICATION_ID)
        val disposition = PlatformTestSupport.app.onMailboxClip(
            FfiClipItem(
                id = UUID.randomUUID().toString(),
                tsMs = System.currentTimeMillis(),
                seq = 1UL,
                content = FfiClipContent.Text("deferred"),
            ),
        )
        assertEquals(MailboxDisposition.DEFERRED, disposition)
        val mailboxVisible = awaitMailboxNotificationState(
            manager = manager,
            expectedVisible = expected == NotificationState.VISIBLE.persistedValue,
        )
        assertEquals(expected == NotificationState.VISIBLE.persistedValue, mailboxVisible)

        if (expected == NotificationState.RESTRICTED.persistedValue) {
            val settings = PlatformTestSupport.device.wait(
                Until.findObject(By.text(PlatformTestSupport.context.getString(R.string.notification_settings))),
                5_000,
            )
            assertNotNull("restricted status did not expose settings entry", settings)
            settings.click()
            assertNotNull(
                "notification settings did not open",
                PlatformTestSupport.device.wait(Until.hasObject(By.pkg("com.android.settings")), 5_000),
            )
        }
    }

    private fun respondToPermissionDialog(grant: Boolean): Boolean {
        val candidates = if (grant) {
            listOf(
                By.res("com.android.permissioncontroller", "permission_allow_button"),
                By.res("com.google.android.permissioncontroller", "permission_allow_button"),
                By.text("Allow"),
                By.text("允许"),
            )
        } else {
            listOf(
                By.res("com.android.permissioncontroller", "permission_deny_button"),
                By.res("com.google.android.permissioncontroller", "permission_deny_button"),
                By.text("Don’t allow"),
                By.text("Don't allow"),
                By.text("不允许"),
            )
        }
        repeat(20) {
            candidates.firstNotNullOfOrNull { selector ->
                PlatformTestSupport.device.findObject(selector)
            }?.let { button ->
                button.click()
                return true
            }
            android.os.SystemClock.sleep(250)
        }
        return false
    }

    private fun awaitMailboxNotificationState(
        manager: NotificationManager,
        expectedVisible: Boolean,
    ): Boolean {
        val deadline = android.os.SystemClock.uptimeMillis() + if (expectedVisible) 5_000 else 1_000
        do {
            val visible = manager.activeNotifications.any { it.id == AppContract.MAILBOX_NOTIFICATION_ID }
            if (visible) return true
            android.os.SystemClock.sleep(100)
        } while (android.os.SystemClock.uptimeMillis() < deadline)
        return false
    }
}
