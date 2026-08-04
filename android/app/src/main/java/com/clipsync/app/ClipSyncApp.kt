package com.clipsync.app

import android.app.Application
import android.content.Context
import android.util.Log
import androidx.annotation.VisibleForTesting
import java.io.File
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import kotlin.math.roundToLong
import kotlin.random.Random
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import uniffi.clipboard_core.CoreCallbacks
import uniffi.clipboard_core.CoreException
import uniffi.clipboard_core.CoreHandle
import uniffi.clipboard_core.CoreStatus
import uniffi.clipboard_core.FfiClipItem
import uniffi.clipboard_core.MailboxDisposition

data class ClipSyncUiState(
    val coreStatus: String = "未配对",
    val notificationState: NotificationState = NotificationState.VISIBLE,
    val pairing: PairingUiState = PairingUiState.Unpaired,
    val history: List<CoreHistoryItem> = emptyList(),
    val historyLoading: Boolean = false,
    val lastError: String? = null,
)

class ClipSyncApp : Application(), CoreCallbacks {
    private val executor = Executors.newSingleThreadScheduledExecutor { runnable ->
        Thread(runnable, "clipsync-android-core")
    }
    private val coreLock = Any()
    private val mutableUiState = MutableStateFlow(ClipSyncUiState())
    private var coreSlot = CoreSlot()

    val uiState = mutableUiState.asStateFlow()

    override fun onCreate() {
        super.onCreate()
        refreshNotificationState()
    }

    fun startCoreAfterForeground() {
        val generation = synchronized(coreLock) {
            if (coreSlot.ready || coreSlot.startScheduled) return
            coreSlot = coreSlot.copy(startScheduled = true)
            coreSlot.generation
        }
        executor.execute {
            val activeGateway = synchronized(coreLock) {
                if (coreSlot.generation != generation || !coreSlot.startScheduled) {
                    null
                } else {
                    coreSlot.gateway ?: createNativeGatewayLocked()
                }
            } ?: return@execute
            try {
                val paired = activeGateway.loadPairingAndStart()
                val pairing = if (paired) activeGateway.pairingSnapshot() else PairingUiState.Unpaired
                val history = normalizeHistory(activeGateway.history())
                mutableUiState.value = mutableUiState.value.copy(
                    coreStatus = if (paired) "连接中" else "未配对",
                    pairing = pairing,
                    history = history,
                    historyLoading = false,
                    lastError = null,
                )
                val pendingSends = synchronized(coreLock) {
                    if (coreSlot.generation != generation) return@execute
                    val pending = coreSlot.pendingSends
                    coreSlot = coreSlot.copy(
                        startScheduled = false,
                        ready = true,
                        retryAttempt = 0,
                        pendingSends = emptyList(),
                    )
                    pending
                }
                pendingSends.forEach { pending ->
                    sendWithGateway(activeGateway, pending.payload, pending.onComplete)
                }
            } catch (error: Throwable) {
                val retryPlan = synchronized(coreLock) {
                    if (coreSlot.generation == generation) {
                        val retryToken = coreSlot.retryAttempt + 1
                        val pending = coreSlot.pendingSends
                        coreSlot = coreSlot.copy(
                            startScheduled = false,
                            ready = false,
                            retryAttempt = retryToken,
                            pendingSends = emptyList(),
                        )
                        CoreRetryPlan(
                            delayMillis = retryDelayMillis(
                                attempt = retryToken - 1,
                                jitter = Random.nextDouble(
                                    from = -RETRY_JITTER_FRACTION,
                                    until = RETRY_JITTER_FRACTION,
                                ),
                            ),
                            retryToken = retryToken,
                            pendingSends = pending,
                        )
                    } else {
                        null
                    }
                }
                retryPlan?.let { plan ->
                    reportError("核心启动失败", error)
                    plan.pendingSends.forEach { it.onComplete(SendResult.ServiceUnavailable) }
                    scheduleCoreRetry(generation, plan.retryToken, plan.delayMillis)
                }
            }
        }
    }

    fun stopCore() {
        val (activeGateway, pendingSends) = synchronized(coreLock) {
            val value = coreSlot.gateway
            val pending = coreSlot.pendingSends
            coreSlot = CoreSlot(generation = coreSlot.generation + 1)
            value to pending
        }
        pendingSends.forEach { it.onComplete(SendResult.ServiceUnavailable) }
        activeGateway ?: return
        executor.execute {
            runCatching { activeGateway.shutdown() }
                .onFailure { Log.w(TAG, "Core shutdown failed", it) }
        }
    }

    fun send(payload: ClipboardPayload, onComplete: (SendResult) -> Unit) {
        var queued = false
        val activeGateway = synchronized(coreLock) {
            if (coreSlot.ready) {
                coreSlot.gateway
            } else if (coreSlot.pendingSends.size < MAX_PENDING_SENDS) {
                coreSlot = coreSlot.copy(
                    pendingSends = coreSlot.pendingSends + PendingSend(payload, onComplete),
                )
                queued = true
                null
            } else {
                null
            }
        }
        if (queued) return
        if (activeGateway == null) {
            onComplete(SendResult.ServiceUnavailable)
            return
        }
        executor.execute { sendWithGateway(activeGateway, payload, onComplete) }
    }

    fun claimPairing(qrPayload: String) {
        mutableUiState.value = mutableUiState.value.copy(
            pairing = PairingUiState.Claiming,
            lastError = null,
        )
        executeWithGateway("无法识别 ClipSync 配对码") { gateway ->
            val sas = gateway.claimPairing(qrPayload)
            mutableUiState.value = mutableUiState.value.copy(
                pairing = PairingUiState.SasReady(sas),
                coreStatus = "等待确认",
                lastError = null,
            )
        }
    }

    fun confirmPairing() {
        val pairing = mutableUiState.value.pairing
        if (!pairing.canConfirm || pairing !is PairingUiState.SasReady) return
        executeWithGateway("配对确认失败") { gateway ->
            gateway.confirmPairing(pairing.sas)
            mutableUiState.value = mutableUiState.value.copy(
                pairing = gateway.pairingSnapshot(),
                coreStatus = "连接中",
                lastError = null,
            )
            refreshHistoryBlocking(gateway)
        }
    }

    fun cancelPairing() {
        executeWithGateway("取消配对失败") { gateway ->
            gateway.cancelPairing()
            mutableUiState.value = mutableUiState.value.copy(
                pairing = PairingUiState.Unpaired,
                coreStatus = "未配对",
                lastError = null,
            )
        }
    }

    fun refreshHistory() {
        mutableUiState.value = mutableUiState.value.copy(historyLoading = true)
        executeWithGateway("读取历史失败") { gateway -> refreshHistoryBlocking(gateway) }
    }

    fun loadHistoryImage(id: String) {
        val item = mutableUiState.value.history.firstOrNull { it.id == id } ?: return
        val image = item.content as? CoreHistoryContent.Image ?: return
        if (image.bytes != null) return
        executeWithGateway("读取历史图片失败") { gateway ->
            val bytes = gateway.historyImageBytes(id)
            mutableUiState.value = mutableUiState.value.copy(
                history = mutableUiState.value.history.map { current ->
                    if (current.id == id) current.copy(content = CoreHistoryContent.Image(bytes)) else current
                },
                lastError = null,
            )
        }
    }

    fun applyHistory(id: String) {
        executeWithGateway("应用历史失败") { gateway ->
            val item = mutableUiState.value.history.firstOrNull { it.id == id }
                ?: error("历史条目不存在")
            val payload = when (val content = item.content) {
                is CoreHistoryContent.Text -> ClipboardPayload.Text(content.text)
                is CoreHistoryContent.Image -> ClipboardPayload.Image(
                    content.bytes ?: gateway.historyImageBytes(item.id),
                )
            }
            LiveClipboardWriter.apply(this, item.id, payload)
            if (item.isDeferred) gateway.applyHistory(item.id)
            mutableUiState.value = mutableUiState.value.copy(
                history = promoteAppliedHistory(mutableUiState.value.history, item.id),
                lastError = null,
            )
        }
    }

    fun clearHistory() {
        executeWithGateway("清空历史失败") { gateway ->
            gateway.clearHistory()
            mutableUiState.value = mutableUiState.value.copy(
                history = emptyList(),
                historyLoading = false,
                lastError = null,
            )
        }
    }

    fun refreshNotificationState() {
        val notificationState = AppContract.notificationState(this)
        preferences().edit()
            .putString(AppContract.KEY_NOTIFICATION_STATE, notificationState.persistedValue)
            .apply()
        mutableUiState.value = mutableUiState.value.copy(notificationState = notificationState)
    }

    fun notificationPermissionWasAsked(): Boolean =
        preferences().getBoolean(AppContract.KEY_NOTIFICATION_ASKED, false)

    fun markNotificationPermissionAsked() {
        preferences().edit().putBoolean(AppContract.KEY_NOTIFICATION_ASKED, true).apply()
    }

    fun reportUiError(message: String) {
        mutableUiState.value = mutableUiState.value.copy(lastError = message)
    }

    fun clearUiError() {
        mutableUiState.value = mutableUiState.value.copy(lastError = null)
    }

    override fun onClip(item: FfiClipItem) {
        try {
            LiveClipboardWriter.apply(this, item)
        } catch (error: Exception) {
            throw CoreException.InvalidInput(error.message ?: "无法写入实时剪贴板")
        }
        scheduleHistoryRefresh()
    }

    override fun onMailboxClip(item: FfiClipItem): MailboxDisposition {
        ClipSyncNotifications.showMailbox(this)
        scheduleHistoryRefresh()
        return MailboxDisposition.DEFERRED
    }

    override fun onStatus(status: CoreStatus) {
        when (status) {
            CoreStatus.ReadyUnpaired -> {
                updateCoreStatus("未配对")
                schedulePairingRefresh()
            }
            CoreStatus.Offering -> updateCoreStatus("等待配对")
            CoreStatus.SasReady -> {
                updateCoreStatus("等待确认")
                schedulePairingRefresh()
            }
            CoreStatus.Connecting -> updateCoreStatus("连接中")
            CoreStatus.Connected -> {
                updateCoreStatus("已连接")
                schedulePairingRefresh()
                scheduleHistoryRefresh()
            }
            CoreStatus.Reconnecting -> updateCoreStatus("重新连接中")
            CoreStatus.Disconnected -> updateCoreStatus("已断开")
            is CoreStatus.Error -> reportError(status.message, null)
        }
    }

    @VisibleForTesting
    fun installCoreGatewayForTest(testGateway: CoreGateway) {
        synchronized(coreLock) {
            check(coreSlot.handle == null) { "native core already initialized" }
            coreSlot = CoreSlot(
                generation = coreSlot.generation + 1,
                gateway = testGateway,
            )
        }
    }

    @VisibleForTesting
    fun hasNativeCoreHandleForTest(): Boolean = synchronized(coreLock) {
        coreSlot.handle != null
    }

    @VisibleForTesting
    fun resetCoreGatewayForTest() {
        synchronized(coreLock) {
            coreSlot = CoreSlot(generation = coreSlot.generation + 1)
        }
    }

    private fun createNativeGatewayLocked(): CoreGateway {
        coreSlot.gateway?.let { return it }
        val stateDirectory = File(filesDir, AppContract.CORE_DIRECTORY).apply { mkdirs() }
        val handle = CoreHandle(stateDirectory.absolutePath, this)
        val nativeGateway = NativeCoreGateway(handle)
        coreSlot = coreSlot.copy(handle = handle, gateway = nativeGateway)
        return nativeGateway
    }

    private fun sendWithGateway(
        gateway: CoreGateway,
        payload: ClipboardPayload,
        onComplete: (SendResult) -> Unit,
    ) {
        val result = runCatching {
            val sequence = when (payload) {
                is ClipboardPayload.Image -> gateway.sendImage(payload.bytes)
                is ClipboardPayload.Text -> gateway.sendText(payload.text)
            }
            SendResult.Sent(sequence)
        }.getOrElse { error ->
            reportError("发送失败", error)
            SendResult.Failed(error.message.orEmpty())
        }
        onComplete(result)
        if (result is SendResult.Sent) refreshHistoryBlocking(gateway)
    }

    private fun executeWithGateway(errorPrefix: String, action: (CoreGateway) -> Unit) {
        executor.execute {
            val gateway = synchronized(coreLock) { coreSlot.gateway }
            if (gateway == null) {
                reportError(errorPrefix, IllegalStateException("同步服务尚未就绪"))
                return@execute
            }
            runCatching { action(gateway) }
                .onFailure { error ->
                    if (mutableUiState.value.pairing == PairingUiState.Claiming) {
                        mutableUiState.value = mutableUiState.value.copy(pairing = PairingUiState.Unpaired)
                    }
                    reportError(errorPrefix, error)
                }
        }
    }

    private fun refreshHistoryBlocking(gateway: CoreGateway) {
        mutableUiState.value = mutableUiState.value.copy(
            history = normalizeHistory(gateway.history()),
            historyLoading = false,
            lastError = null,
        )
    }

    private fun scheduleHistoryRefresh() {
        executor.schedule(
            {
                synchronized(coreLock) { coreSlot.gateway }?.let { gateway ->
                    runCatching { refreshHistoryBlocking(gateway) }
                        .onFailure { reportError("读取历史失败", it) }
                }
            },
            CALLBACK_SETTLE_DELAY_MILLIS,
            TimeUnit.MILLISECONDS,
        )
    }

    private fun schedulePairingRefresh() {
        executor.schedule(
            {
                synchronized(coreLock) { coreSlot.gateway }?.let { gateway ->
                    runCatching {
                        mutableUiState.value = mutableUiState.value.copy(
                            pairing = gateway.pairingSnapshot(),
                        )
                    }.onFailure { reportError("读取配对状态失败", it) }
                }
            },
            CALLBACK_SETTLE_DELAY_MILLIS,
            TimeUnit.MILLISECONDS,
        )
    }

    private fun scheduleCoreRetry(generation: Long, retryToken: Int, delayMillis: Long) {
        executor.schedule(
            {
                val shouldRetry = synchronized(coreLock) {
                    coreSlot.generation == generation &&
                        coreSlot.retryAttempt == retryToken &&
                        !coreSlot.ready &&
                        !coreSlot.startScheduled
                }
                if (shouldRetry) startCoreAfterForeground()
            },
            delayMillis,
            TimeUnit.MILLISECONDS,
        )
    }

    private fun updateCoreStatus(value: String) {
        preferences().edit().putString(AppContract.KEY_CORE_STATUS, value).apply()
        mutableUiState.value = mutableUiState.value.copy(coreStatus = value, lastError = null)
    }

    private fun reportError(prefix: String, error: Throwable?) {
        val detail = error?.message?.takeIf { it.isNotBlank() }
        val message = if (detail == null) prefix else "$prefix：$detail"
        Log.e(TAG, message, error)
        preferences().edit().putString(AppContract.KEY_CORE_STATUS, message).apply()
        mutableUiState.value = mutableUiState.value.copy(coreStatus = "错误", lastError = message)
    }

    private fun preferences() = getSharedPreferences(AppContract.PREFERENCES, Context.MODE_PRIVATE)

    companion object {
        private const val CALLBACK_SETTLE_DELAY_MILLIS = 100L
        private const val MAX_PENDING_SENDS = 8
        private const val TAG = "ClipSyncApp"
    }

    private data class PendingSend(
        val payload: ClipboardPayload,
        val onComplete: (SendResult) -> Unit,
    )

    private data class CoreRetryPlan(
        val delayMillis: Long,
        val retryToken: Int,
        val pendingSends: List<PendingSend>,
    )

    private data class CoreSlot(
        val generation: Long = 0,
        val startScheduled: Boolean = false,
        val ready: Boolean = false,
        val retryAttempt: Int = 0,
        val handle: CoreHandle? = null,
        val gateway: CoreGateway? = null,
        val pendingSends: List<PendingSend> = emptyList(),
    )
}

private const val MAX_RETRY_SECONDS = 30L
private const val RETRY_JITTER_FRACTION = 0.2

internal fun retryDelayMillis(attempt: Int, jitter: Double): Long {
    val seconds = if (attempt >= 5) MAX_RETRY_SECONDS else 1L shl attempt.coerceAtLeast(0)
    val factor = 1.0 + jitter.coerceIn(-RETRY_JITTER_FRACTION, RETRY_JITTER_FRACTION)
    return (seconds * 1_000.0 * factor).roundToLong()
}
