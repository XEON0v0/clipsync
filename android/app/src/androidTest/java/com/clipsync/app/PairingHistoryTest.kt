package com.clipsync.app

import android.os.SystemClock
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class PairingHistoryTest {
    @Before
    fun setUp() {
        PlatformTestSupport.resetFixture(installGateway = false)
        PlatformTestSupport.installUnpairedGateway()
    }

    @After
    fun tearDown() = PlatformTestSupport.stopFixture()

    @Test
    fun nonProtocolQrReturnsFriendlyErrorAndUnpairedState() {
        PlatformTestSupport.app.claimPairing("https://example.invalid/not-clipsync")

        val state = awaitState { it.lastError?.contains("无法识别 ClipSync 配对码") == true }

        assertEquals(PairingUiState.Unpaired, state.pairing)
        assertNull(
            PlatformTestSupport.preferences.getString(
                PlatformTestSupport.KEY_TEST_PAIR_CONFIRMED,
                null,
            ),
        )
    }

    @Test
    fun scannedPairingIsNotConfirmedOrDurableBeforeSasApproval() {
        PlatformTestSupport.app.claimPairing(PlatformTestSupport.VALID_TEST_QR)

        val state = awaitState { it.pairing is PairingUiState.SasReady }

        assertEquals(PairingUiState.SasReady("123456"), state.pairing)
        assertNull(
            PlatformTestSupport.preferences.getString(
                PlatformTestSupport.KEY_TEST_PAIR_CONFIRMED,
                null,
            ),
        )
        PlatformTestSupport.app.cancelPairing()
        awaitState { it.pairing == PairingUiState.Unpaired }
        assertNull(
            PlatformTestSupport.preferences.getString(
                PlatformTestSupport.KEY_TEST_PAIR_CONFIRMED,
                null,
            ),
        )
    }

    @Test
    fun sasApprovalConfirmsUsingTheExactCoreCode() {
        PlatformTestSupport.app.claimPairing(PlatformTestSupport.VALID_TEST_QR)
        awaitState { it.pairing == PairingUiState.SasReady("123456") }

        PlatformTestSupport.app.confirmPairing()

        awaitState { it.pairing is PairingUiState.Paired }
        assertEquals(
            "123456",
            PlatformTestSupport.preferences.getString(
                PlatformTestSupport.KEY_TEST_PAIR_CONFIRMED,
                null,
            ),
        )
    }

    @Test
    fun deferredHistoryIsPromotedOnlyAfterApplyingToClipboard() {
        PlatformTestSupport.installUnpairedGateway(withDeferredHistory = true)
        PlatformTestSupport.app.refreshHistory()
        val deferred = awaitState { it.history.singleOrNull()?.isDeferred == true }.history.single()

        PlatformTestSupport.app.applyHistory(deferred.id)

        val applied = awaitState { it.history.singleOrNull()?.source == CoreHistorySource.REMOTE }
        assertEquals(CoreHistorySource.REMOTE, applied.history.single().source)
        assertEquals(
            deferred.id,
            PlatformTestSupport.preferences.getString(
                PlatformTestSupport.KEY_TEST_HISTORY_APPLIED,
                null,
            ),
        )
    }

    private fun awaitState(predicate: (ClipSyncUiState) -> Boolean): ClipSyncUiState {
        val deadline = SystemClock.uptimeMillis() + 10_000
        while (SystemClock.uptimeMillis() < deadline) {
            val state = PlatformTestSupport.app.uiState.value
            if (predicate(state)) return state
            SystemClock.sleep(50)
        }
        val state = PlatformTestSupport.app.uiState.value
        assertTrue("timed out waiting for state: $state", predicate(state))
        return state
    }
}
