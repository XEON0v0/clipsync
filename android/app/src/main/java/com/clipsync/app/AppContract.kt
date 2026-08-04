package com.clipsync.app

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build

enum class NotificationState(val persistedValue: String) {
    RESTRICTED("restricted"),
    VISIBLE("visible"),
}

object AppContract {
    const val CORE_DIRECTORY = "core"
    const val FILE_PROVIDER_AUTHORITY_SUFFIX = ".files"
    const val HISTORY_LIMIT = 50
    const val MAILBOX_CHANNEL = "clipsync_mailbox"
    const val MAILBOX_NOTIFICATION_ID = 1002
    const val MAX_IMAGE_BYTES = 10 * 1024 * 1024
    const val MAX_IMAGE_PIXELS = 50_000_000L
    const val MAX_RGBA_BYTES = 256L * 1024L * 1024L
    const val PREFERENCES = "clipsync_state"
    const val SERVICE_CHANNEL = "clipsync_data_sync"
    const val SERVICE_NOTIFICATION_ID = 1001

    const val KEY_CORE_STATUS = "core_status"
    const val KEY_LAST_SEND_RESULT = "last_send_result"
    const val KEY_NOTIFICATION_ASKED = "notification_asked"
    const val KEY_NOTIFICATION_STATE = "notification_state"
    const val KEY_SERVICE_RUNNING = "service_running"
    const val KEY_WINDOW_FOCUS_READ = "window_focus_read"

    fun notificationState(context: Context): NotificationState {
        val granted = Build.VERSION.SDK_INT < 33 ||
            context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) ==
            PackageManager.PERMISSION_GRANTED
        return if (granted) NotificationState.VISIBLE else NotificationState.RESTRICTED
    }
}
