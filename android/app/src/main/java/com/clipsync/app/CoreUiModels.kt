package com.clipsync.app

sealed interface PairingUiState {
    val canConfirm: Boolean
        get() = this is SasReady && sas.length == 6 && sas.all(Char::isDigit)

    data object Unpaired : PairingUiState
    data object Claiming : PairingUiState
    data class SasReady(val sas: String) : PairingUiState
    data class Paired(val roomId: String) : PairingUiState
}

sealed interface CoreHistoryContent {
    data class Text(val text: String) : CoreHistoryContent
    data class Image(val bytes: ByteArray? = null) : CoreHistoryContent
}

enum class CoreHistorySource {
    LOCAL,
    REMOTE,
    REMOTE_DEFERRED,
}

data class CoreHistoryItem(
    val id: String,
    val tsMs: Long,
    val content: CoreHistoryContent,
    val source: CoreHistorySource,
) {
    val isDeferred: Boolean
        get() = source == CoreHistorySource.REMOTE_DEFERRED

    val sourceLabel: String
        get() = when (source) {
            CoreHistorySource.LOCAL -> "本机"
            CoreHistorySource.REMOTE -> "另一台设备"
            CoreHistorySource.REMOTE_DEFERRED -> "离线收到，点按应用"
        }
}

internal fun normalizeHistory(items: List<CoreHistoryItem>): List<CoreHistoryItem> =
    items.sortedByDescending(CoreHistoryItem::tsMs).take(AppContract.HISTORY_LIMIT)

internal fun promoteAppliedHistory(
    items: List<CoreHistoryItem>,
    selectedId: String,
): List<CoreHistoryItem> = items.map { item ->
    if (item.id == selectedId && item.isDeferred) {
        item.copy(source = CoreHistorySource.REMOTE)
    } else {
        item
    }
}
