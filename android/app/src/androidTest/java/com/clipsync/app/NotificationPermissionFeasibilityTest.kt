package com.clipsync.app

import android.Manifest
import android.content.pm.PackageManager
import android.os.Build
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.uiautomator.By
import androidx.test.uiautomator.Until
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

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
    fun dataSyncServiceRunsAndStatusSurfaceMatchesNotificationPermission() {
        val expected = InstrumentationRegistry.getArguments().getString("expectedNotificationState")
            ?: throw AssertionError("expectedNotificationState instrumentation argument is required")
        val permissionGranted = PlatformTestSupport.context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) ==
            PackageManager.PERMISSION_GRANTED
        assertEquals(expected == SpikeContract.NOTIFICATION_VISIBLE, permissionGranted)

        val serviceState = PlatformTestSupport.awaitStringPreference(SpikeContract.KEY_SERVICE_RUNNING) {
            PlatformTestSupport.startForegroundService(SpikeContract.ACTION_HEALTH_CHECK)
        }
        assertEquals("true", serviceState)
        assertEquals(expected, PlatformTestSupport.preferences.getString(SpikeContract.KEY_NOTIFICATION_STATE, null))

        PlatformTestSupport.shell("am start -W -n com.clipsync.app/.MainActivity")
        val status = PlatformTestSupport.device.wait(
            Until.findObject(By.text("Notification visibility: $expected")),
            10_000,
        )
        assertNotNull("status surface did not report $expected", status)
    }
}
