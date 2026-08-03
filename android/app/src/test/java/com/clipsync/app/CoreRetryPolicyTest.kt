package com.clipsync.app

import org.junit.Assert.assertEquals
import org.junit.Test

class CoreRetryPolicyTest {
    @Test
    fun zeroJitterUsesExponentialLadderWithThirtySecondCap() {
        val delays = (0..6).map { retryDelayMillis(it, jitter = 0.0) }

        assertEquals(
            listOf(1_000L, 2_000L, 4_000L, 8_000L, 16_000L, 30_000L, 30_000L),
            delays,
        )
    }

    @Test
    fun jitterIsClampedToPlusOrMinusTwentyPercent() {
        assertEquals(800L, retryDelayMillis(attempt = 0, jitter = -0.5))
        assertEquals(1_200L, retryDelayMillis(attempt = 0, jitter = 0.5))
        assertEquals(24_000L, retryDelayMillis(attempt = 31, jitter = -0.5))
        assertEquals(36_000L, retryDelayMillis(attempt = 31, jitter = 0.5))
    }
}
