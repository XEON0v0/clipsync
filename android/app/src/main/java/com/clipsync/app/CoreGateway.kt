package com.clipsync.app

import uniffi.clipboard_core.CoreHandle
import uniffi.clipboard_core.FfiHistoryKind
import uniffi.clipboard_core.FfiHistorySource
import uniffi.clipboard_core.PairingSnapshot

interface CoreGateway {
    fun loadPairingAndStart(): Boolean
    fun pairingSnapshot(): PairingUiState
    fun claimPairing(qrPayload: String): String
    fun confirmPairing(sas: String)
    fun cancelPairing()
    fun history(): List<CoreHistoryItem>
    fun historyImageBytes(id: String): ByteArray
    fun applyHistory(id: String)
    fun clearHistory()
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

    override fun pairingSnapshot(): PairingUiState = when (val snapshot = handle.pairPoll()) {
        PairingSnapshot.Unpaired,
        is PairingSnapshot.Offering,
        -> PairingUiState.Unpaired
        is PairingSnapshot.SasReady -> PairingUiState.SasReady(snapshot.sas)
        is PairingSnapshot.Paired -> PairingUiState.Paired(snapshot.roomId)
    }

    override fun claimPairing(qrPayload: String): String = handle.pairClaim(qrPayload)

    override fun confirmPairing(sas: String) = handle.pairConfirm(sas)

    override fun cancelPairing() = handle.pairCancel()

    override fun history(): List<CoreHistoryItem> = handle.history().map { item ->
        CoreHistoryItem(
            id = item.id,
            tsMs = item.tsMs,
            content = when (val kind = item.kind) {
                is FfiHistoryKind.Text -> CoreHistoryContent.Text(kind.content)
                FfiHistoryKind.Image -> CoreHistoryContent.Image()
            },
            source = when (item.source) {
                FfiHistorySource.LOCAL -> CoreHistorySource.LOCAL
                FfiHistorySource.REMOTE -> CoreHistorySource.REMOTE
                FfiHistorySource.REMOTE_DEFERRED -> CoreHistorySource.REMOTE_DEFERRED
            },
        )
    }

    override fun historyImageBytes(id: String): ByteArray = handle.historyImageBytes(id)

    override fun applyHistory(id: String) = handle.historyApply(id)

    override fun clearHistory() = handle.historyClear()

    override fun sendImage(bytes: ByteArray): ULong = handle.sendImage(bytes)

    override fun sendText(text: String): ULong = handle.sendText(text)

    override fun shutdown() = handle.shutdown()
}
