package com.clipsync.app

import android.os.Build
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.uiautomator.By
import androidx.test.uiautomator.Until
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class Api35CompatibilityTest {
    @Before
    fun givenCleanApi35Fixture() {
        org.junit.Assume.assumeTrue("compatibility fixture requires API 35", Build.VERSION.SDK_INT == 35)
        PlatformTestSupport.resetFixture()
    }

    @After
    fun cleanupApi35Fixture() {
        PlatformTestSupport.stopFixture()
    }

    @Test
    fun target34ApplicationBootsAndDataSyncServiceRunsOnApi35() {
        assertEquals("1", PlatformTestSupport.shell("getprop sys.boot_completed"))
        assertEquals(34, PlatformTestSupport.context.applicationInfo.targetSdkVersion)

        PlatformTestSupport.shell("am start -W -n com.clipsync.app/.MainActivity")
        assertNotNull(
            "target-34 application did not boot on API 35",
            PlatformTestSupport.device.wait(Until.findObject(By.text("ClipSync")), 10_000),
        )
        PlatformTestSupport.device.pressHome()

        val serviceState = PlatformTestSupport.awaitStringPreference(SpikeContract.KEY_SERVICE_RUNNING) {
            PlatformTestSupport.startForegroundService(SpikeContract.ACTION_HEALTH_CHECK)
        }
        assertEquals("true", serviceState)
    }
}
