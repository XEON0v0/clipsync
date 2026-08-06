package com.clipsync.app

import android.os.Build
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.uiautomator.By
import androidx.test.uiautomator.Until
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class Api35CompatibilityTest {
    @Before
    fun givenCleanApi35Fixture() {
        org.junit.Assume.assumeTrue("compatibility fixture requires API 35", Build.VERSION.SDK_INT == 35)
        PlatformTestSupport.resetFixture()
        // Same precondition as the API 34 boot harness: notification permission is
        // granted up front so the first-launch permission dialog does not cover the
        // activity under test.
        PlatformTestSupport.shell("pm grant com.clipsync.app android.permission.POST_NOTIFICATIONS")
    }

    @After
    fun cleanupApi35Fixture() {
        PlatformTestSupport.stopFixture()
    }

    @Test
    fun target34ApplicationBootsAndDataSyncServiceRunsOnApi35() {
        assertEquals("1", PlatformTestSupport.shell("getprop sys.boot_completed"))
        assertEquals(34, PlatformTestSupport.context.applicationInfo.targetSdkVersion)

        PlatformTestSupport.launchMainActivity()
        assertNotNull(
            "target-34 application did not boot on API 35",
            PlatformTestSupport.device.wait(Until.findObject(By.text("ClipSync")), 10_000),
        )
        assertTrue(PlatformTestSupport.awaitBooleanPreference(AppContract.KEY_SERVICE_RUNNING))
    }
}
