package com.clipsync.app

import uniffi.clipboard_core.CoreHandle

interface CoreGateway {
    fun loadPairingAndStart(): Boolean
    fun sendImage(bytes: ByteArray): ULong
    fun sendText(text: String): ULong
    fun shutdown()
}

class NativeCoreGateway(private val handle: CoreHandle) : CoreGateway {
    override fun loadPairingAndStart(): Boolean {
        val paired = handle.pairLoad()
        if (paired) {
            handle.start()
        }
        return paired
    }

    override fun sendImage(bytes: ByteArray): ULong = handle.sendImage(bytes)

    override fun sendText(text: String): ULong = handle.sendText(text)

    override fun shutdown() = handle.shutdown()
}
