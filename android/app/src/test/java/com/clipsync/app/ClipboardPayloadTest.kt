package com.clipsync.app

import java.io.ByteArrayInputStream
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class ClipboardPayloadTest {
    @Test
    fun boundedReaderAcceptsExactlyTenMiB() {
        val bytes = ByteArray(AppContract.MAX_IMAGE_BYTES) { (it % 251).toByte() }

        val result = readAtMost(ByteArrayInputStream(bytes), AppContract.MAX_IMAGE_BYTES)

        assertArrayEquals(bytes, result)
    }

    @Test
    fun boundedReaderRejectsElevenMiBBeforeSend() {
        val bytes = ByteArray(11 * 1024 * 1024)

        val result = readAtMost(ByteArrayInputStream(bytes), AppContract.MAX_IMAGE_BYTES)

        assertNull(result)
    }

    @Test
    fun sendResultsHaveStableDiagnosticValues() {
        assertEquals("sent:7", SendResult.Sent(7UL).preferenceValue())
        assertEquals("oversize", SendResult.Oversize.preferenceValue())
        assertEquals("service_unavailable", SendResult.ServiceUnavailable.preferenceValue())
    }
}
