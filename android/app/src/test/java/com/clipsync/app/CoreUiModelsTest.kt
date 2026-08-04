package com.clipsync.app

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class CoreUiModelsTest {
    @Test
    fun sasConfirmationIsAvailableOnlyForSixDigitCoreSnapshot() {
        assertFalse(PairingUiState.Unpaired.canConfirm)
        assertFalse(PairingUiState.Claiming.canConfirm)
        assertFalse(PairingUiState.SasReady("12345").canConfirm)
        assertFalse(PairingUiState.SasReady("12345a").canConfirm)
        assertTrue(PairingUiState.SasReady("123456").canConfirm)
    }

    @Test
    fun historySnapshotIsNewestFirstAndKeepsDeferredSource() {
        val entries = normalizeHistory(
            listOf(
                CoreHistoryItem(
                    id = "older",
                    tsMs = 10,
                    content = CoreHistoryContent.Text("first"),
                    source = CoreHistorySource.LOCAL,
                ),
                CoreHistoryItem(
                    id = "newer",
                    tsMs = 20,
                    content = CoreHistoryContent.Image(byteArrayOf(1, 2, 3)),
                    source = CoreHistorySource.REMOTE_DEFERRED,
                ),
            ),
        )

        assertEquals(listOf("newer", "older"), entries.map(CoreHistoryItem::id))
        assertTrue(entries.first().isDeferred)
        assertEquals("离线收到，点按应用", entries.first().sourceLabel)
        assertEquals("本机", entries.last().sourceLabel)
    }

    @Test
    fun successfulDeferredApplyPromotesOnlyTheSelectedEntry() {
        val entries = listOf(
            CoreHistoryItem(
                id = "selected",
                tsMs = 20,
                content = CoreHistoryContent.Text("selected"),
                source = CoreHistorySource.REMOTE_DEFERRED,
            ),
            CoreHistoryItem(
                id = "other",
                tsMs = 10,
                content = CoreHistoryContent.Text("other"),
                source = CoreHistorySource.REMOTE_DEFERRED,
            ),
        )

        val promoted = promoteAppliedHistory(entries, "selected")

        assertEquals(CoreHistorySource.REMOTE, promoted.first().source)
        assertEquals(CoreHistorySource.REMOTE_DEFERRED, promoted.last().source)
    }
}
