package com.clipsync.app

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build

object SpikeContract {
    const val ACTION_HEALTH_CHECK = "com.clipsync.app.spike.HEALTH_CHECK"
    const val ACTION_PROBE_BACKGROUND_READ = "com.clipsync.app.spike.PROBE_BACKGROUND_READ"
    const val ACTION_WRITE_CLIPBOARD = "com.clipsync.app.spike.WRITE_CLIPBOARD"
    const val EXTRA_CLIPBOARD_TEXT = "clipboard_text"
    const val KEY_BACKGROUND_READ = "background_read"
    const val KEY_FOCUS_READ = "focus_read"
    const val KEY_HAS_WINDOW_FOCUS = "has_window_focus"
    const val KEY_LAST_WRITE = "last_write"
    const val KEY_NOTIFICATION_STATE = "notification_state"
    const val KEY_READ_TRIGGER = "read_trigger"
    const val KEY_SERVICE_RUNNING = "service_running"
    const val KEY_TILE_LAUNCH_USED = "tile_launch_used"
    const val KEY_TILE_MODE = "tile_mode"
    const val NOTIFICATION_CHANNEL = "clipsync_spike_data_sync"
    const val NOTIFICATION_ID = 18
    const val NOTIFICATION_RESTRICTED = "restricted"
    const val NOTIFICATION_VISIBLE = "visible"
    const val PREFERENCES = "clipsync_platform_spike"
    const val TILE_LABEL = "ClipSync"
    const val TILE_LAUNCH_INTENT_BLOCKED = "intent_blocked"
    const val TILE_MODE_INTENT = "intent"
    const val TILE_MODE_PENDING_INTENT = "pending_intent"

    fun notificationState(context: Context): String {
        val permissionGranted = Build.VERSION.SDK_INT < 33 ||
            context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED
        return if (permissionGranted) NOTIFICATION_VISIBLE else NOTIFICATION_RESTRICTED
    }
}
