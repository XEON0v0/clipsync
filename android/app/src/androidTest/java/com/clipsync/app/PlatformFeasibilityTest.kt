package com.clipsync.app

import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.graphics.Bitmap
import android.os.Build
import androidx.test.ext.junit.runners.AndroidJUnit4
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.FileOutputStream
import java.util.UUID
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import uniffi.clipboard_core.FfiClipContent
import uniffi.clipboard_core.FfiClipItem
import uniffi.clipboard_core.MailboxDisposition

@RunWith(AndroidJUnit4::class)
class PlatformFeasibilityTest {
    @Before
    fun givenCleanPlatformFixture() {
        PlatformTestSupport.resetFixture()
    }

    @After
    fun cleanupPlatformFixture() {
        PlatformTestSupport.stopFixture()
    }

    @Test
    fun manifestHasRequiredForegroundPermissionsAndNoForbiddenCapabilities() {
        val packageInfo = PlatformTestSupport.context.packageManager.getPackageInfo(
            PlatformTestSupport.context.packageName,
            PackageManager.GET_PERMISSIONS,
        )
        val requiredPermissions = setOf(
            android.Manifest.permission.FOREGROUND_SERVICE,
            android.Manifest.permission.FOREGROUND_SERVICE_DATA_SYNC,
            android.Manifest.permission.POST_NOTIFICATIONS,
        )
        val requestedPermissions = packageInfo.requestedPermissions?.toSet().orEmpty()
        assertTrue(requestedPermissions.containsAll(requiredPermissions))
        assertFalse(requestedPermissions.contains(android.Manifest.permission.SYSTEM_ALERT_WINDOW))
        assertFalse(requestedPermissions.contains("android.permission.BIND_ACCESSIBILITY_SERVICE"))
        val serviceInfo = PlatformTestSupport.context.packageManager.getServiceInfo(
            android.content.ComponentName(
                PlatformTestSupport.context,
                ClipboardSyncService::class.java,
            ),
            0,
        )
        assertEquals(ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC, serviceInfo.foregroundServiceType)
    }

    @Test
    fun nativeCoreHandleLoadsOnlyAfterForegroundServiceStarts() {
        PlatformTestSupport.stopFixture()
        PlatformTestSupport.resetFixture(installGateway = false)
        assertFalse(PlatformTestSupport.app.hasNativeCoreHandleForTest())

        PlatformTestSupport.startForegroundService()

        val deadline = android.os.SystemClock.uptimeMillis() + 15_000
        while (!PlatformTestSupport.app.hasNativeCoreHandleForTest() &&
            android.os.SystemClock.uptimeMillis() < deadline
        ) {
            android.os.SystemClock.sleep(100)
        }
        assertTrue("native CoreHandle did not initialize after startForeground", PlatformTestSupport.app.hasNativeCoreHandleForTest())
    }

    @Test
    fun foregroundServiceRetriesTransientCoreStartupFailureAndRecoversSending() {
        PlatformTestSupport.stopFixture()
        PlatformTestSupport.resetFixture(installGateway = false)
        PlatformTestSupport.installFailOnceGateway()

        PlatformTestSupport.startForegroundService()
        assertTrue(
            "core did not retry after its transient startup failure",
            PlatformTestSupport.awaitIntPreferenceAtLeast(
                PlatformTestSupport.KEY_TEST_GATEWAY_START_ATTEMPTS,
                2,
            ),
        )

        val expected = "recovered-api-${Build.VERSION.SDK_INT}"
        PlatformTestSupport.setClipboardText(expected)
        val result = PlatformTestSupport.awaitStringPreference(AppContract.KEY_LAST_SEND_RESULT) {
            PlatformTestSupport.launchFocusActivity()
        }
        assertEquals("sent:42", result)
        assertEquals(
            expected,
            PlatformTestSupport.preferences.getString(PlatformTestSupport.KEY_TEST_SENT_TEXT, null),
        )
        android.os.SystemClock.sleep(250)
        assertEquals(
            "starting the existing service must not restart a ready core",
            2,
            PlatformTestSupport.preferences.getInt(
                PlatformTestSupport.KEY_TEST_GATEWAY_START_ATTEMPTS,
                0,
            ),
        )
    }

    @Test
    fun liveCallbackWritesClipboardAndFocusedActivitySendsIt() {
        PlatformTestSupport.startForegroundService()
        val expected = "live-api-${Build.VERSION.SDK_INT}"
        PlatformTestSupport.app.onClip(clipItem(FfiClipContent.Text(expected)))

        val result = PlatformTestSupport.awaitStringPreference(AppContract.KEY_LAST_SEND_RESULT) {
            PlatformTestSupport.launchFocusActivity()
        }

        assertEquals("sent:42", result)
        assertEquals(expected, PlatformTestSupport.preferences.getString(PlatformTestSupport.KEY_TEST_SENT_TEXT, null))
        assertTrue(PlatformTestSupport.preferences.getBoolean(AppContract.KEY_WINDOW_FOCUS_READ, false))
    }

    @Test
    fun mailboxCallbackIsAlwaysDeferredAndDoesNotChangeClipboard() {
        PlatformTestSupport.startForegroundService()
        val expected = "mailbox-baseline-api-${Build.VERSION.SDK_INT}"
        PlatformTestSupport.setClipboardText(expected)

        val disposition = PlatformTestSupport.app.onMailboxClip(
            clipItem(FfiClipContent.Text("must-not-apply")),
        )
        assertEquals(MailboxDisposition.DEFERRED, disposition)

        PlatformTestSupport.awaitStringPreference(AppContract.KEY_LAST_SEND_RESULT) {
            PlatformTestSupport.launchFocusActivity()
        }
        assertEquals(expected, PlatformTestSupport.preferences.getString(PlatformTestSupport.KEY_TEST_SENT_TEXT, null))
    }

    @Test
    fun realSystemTileColdStartsServiceThenReadsAfterWindowFocusAndSendsText() {
        val expected = "tile-api-${Build.VERSION.SDK_INT}"
        PlatformTestSupport.setClipboardText(expected)
        PlatformTestSupport.provisionTile()

        val result = PlatformTestSupport.awaitStringPreference(AppContract.KEY_LAST_SEND_RESULT, attempts = 3) {
            PlatformTestSupport.clickProvisionedTile()
        }

        assertEquals("sent:42", result)
        assertEquals(expected, PlatformTestSupport.preferences.getString(PlatformTestSupport.KEY_TEST_SENT_TEXT, null))
        assertTrue(PlatformTestSupport.preferences.getBoolean(AppContract.KEY_WINDOW_FOCUS_READ, false))
        assertTrue(PlatformTestSupport.preferences.getBoolean(AppContract.KEY_SERVICE_RUNNING, false))
    }

    @Test
    fun elevenMiBImageUsesOversizeToastPathWithoutCallingCore() {
        PlatformTestSupport.startForegroundService()
        val directory = File(PlatformTestSupport.context.filesDir, "received_images").apply { mkdirs() }
        val file = File(directory, "oversize-test.png")
        FileOutputStream(file).use { output ->
            val block = ByteArray(1024 * 1024)
            repeat(11) { output.write(block) }
        }
        PlatformTestSupport.setClipboardImage(PlatformTestSupport.fileProviderUri(file))

        val result = PlatformTestSupport.launchFocusActivityAndAwaitToast(R.string.send_oversize)

        assertEquals("oversize", result)
        assertEquals(-1, PlatformTestSupport.preferences.getInt(PlatformTestSupport.KEY_TEST_SENT_IMAGE_SIZE, -1))
        file.delete()
    }

    @Test
    fun liveImageCallbackWritesProviderUriAndFocusedActivitySendsBytes() {
        PlatformTestSupport.startForegroundService()
        val bitmap = Bitmap.createBitmap(2, 2, Bitmap.Config.ARGB_8888)
        val bytes = ByteArrayOutputStream().use { output ->
            bitmap.compress(Bitmap.CompressFormat.PNG, 100, output)
            output.toByteArray()
        }
        bitmap.recycle()
        PlatformTestSupport.app.onClip(clipItem(FfiClipContent.Image(bytes)))

        val result = PlatformTestSupport.awaitStringPreference(AppContract.KEY_LAST_SEND_RESULT) {
            PlatformTestSupport.launchFocusActivity()
        }

        assertEquals("sent:42", result)
        assertEquals(bytes.size, PlatformTestSupport.preferences.getInt(PlatformTestSupport.KEY_TEST_SENT_IMAGE_SIZE, -1))
    }

    private fun clipItem(content: FfiClipContent) = FfiClipItem(
        id = UUID.randomUUID().toString(),
        tsMs = System.currentTimeMillis(),
        seq = 1UL,
        content = content,
    )
}
