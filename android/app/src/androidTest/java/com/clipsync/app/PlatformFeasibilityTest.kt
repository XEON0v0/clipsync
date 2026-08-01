package com.clipsync.app

import android.os.Build
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.After
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

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
    fun backgroundReadIsRestrictedWhenApplicationHasNoFocus() {
        val expected = "background-api-${Build.VERSION.SDK_INT}"
        PlatformTestSupport.awaitStringPreference(SpikeContract.KEY_LAST_WRITE) {
            PlatformTestSupport.startForegroundService(SpikeContract.ACTION_WRITE_CLIPBOARD, expected)
        }

        val result = PlatformTestSupport.awaitStringPreference(SpikeContract.KEY_BACKGROUND_READ) {
            PlatformTestSupport.startForegroundService(SpikeContract.ACTION_PROBE_BACKGROUND_READ)
        }

        assertTrue("background clipboard read unexpectedly returned $result", result == "null" || result == "security_exception")
    }

    @Test
    fun foregroundDataSyncServiceWritesClipboardWhenApplicationHasNoFocus() {
        val expected = "fgs-write-api-${Build.VERSION.SDK_INT}"
        PlatformTestSupport.awaitStringPreference(SpikeContract.KEY_LAST_WRITE) {
            PlatformTestSupport.startForegroundService(SpikeContract.ACTION_WRITE_CLIPBOARD, expected)
        }

        PlatformTestSupport.awaitStringPreference(SpikeContract.KEY_FOCUS_READ) {
            PlatformTestSupport.launchFocusActivity()
        }

        PlatformTestSupport.assertFocusRead(expected)
    }

    @Test
    fun realSystemTileIntentOverloadReadsClipboardAfterWindowFocus() {
        // Apps targeting SDK 34 may no longer launch activities from a tile
        // with a raw Intent; that overload is only exercisable below API 34.
        org.junit.Assume.assumeTrue("Intent overload is blocked for targetSdk 34", Build.VERSION.SDK_INT < 34)
        assertTileRoundTrip(SpikeContract.TILE_MODE_INTENT)
    }

    @Test
    fun realSystemTileIntentOverloadIsRejectedByPlatformOnApi34() {
        org.junit.Assume.assumeTrue("rejection applies to targetSdk 34 on API 34+", Build.VERSION.SDK_INT >= 34)
        PlatformTestSupport.preferences.edit()
            .putString(SpikeContract.KEY_TILE_MODE, SpikeContract.TILE_MODE_INTENT)
            .commit()
        PlatformTestSupport.provisionTile()

        val launchUsed = PlatformTestSupport.awaitStringPreference(SpikeContract.KEY_TILE_LAUNCH_USED, attempts = 3) {
            PlatformTestSupport.clickProvisionedTile()
        }
        org.junit.Assert.assertEquals(SpikeContract.TILE_LAUNCH_INTENT_BLOCKED, launchUsed)
    }

    @Test
    fun realSystemTilePendingIntentOverloadReadsClipboardAfterWindowFocus() {
        org.junit.Assume.assumeTrue("PendingIntent overload requires API 34", Build.VERSION.SDK_INT >= 34)
        assertTileRoundTrip(SpikeContract.TILE_MODE_PENDING_INTENT)
    }

    private fun assertTileRoundTrip(tileMode: String) {
        val expected = "tile-$tileMode-api-${Build.VERSION.SDK_INT}"
        PlatformTestSupport.preferences.edit().putString(SpikeContract.KEY_TILE_MODE, tileMode).commit()
        PlatformTestSupport.awaitStringPreference(SpikeContract.KEY_LAST_WRITE) {
            PlatformTestSupport.startForegroundService(SpikeContract.ACTION_WRITE_CLIPBOARD, expected)
        }
        PlatformTestSupport.provisionTile()

        PlatformTestSupport.awaitStringPreference(SpikeContract.KEY_FOCUS_READ, attempts = 3) {
            PlatformTestSupport.clickProvisionedTile()
        }

        PlatformTestSupport.assertFocusRead(expected)
        org.junit.Assert.assertEquals(tileMode, PlatformTestSupport.preferences.getString(SpikeContract.KEY_TILE_LAUNCH_USED, null))
    }
}
