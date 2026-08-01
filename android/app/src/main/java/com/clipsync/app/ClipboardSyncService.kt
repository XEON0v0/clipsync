package com.clipsync.app

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.os.IBinder

class ClipboardSyncService : Service() {
    override fun onCreate() {
        super.onCreate()
        val notificationManager = getSystemService(NotificationManager::class.java)
        notificationManager.createNotificationChannel(
            NotificationChannel(
                SpikeContract.NOTIFICATION_CHANNEL,
                "ClipSync spike service",
                NotificationManager.IMPORTANCE_DEFAULT,
            ),
        )
        val notification = Notification.Builder(this, SpikeContract.NOTIFICATION_CHANNEL)
            .setContentTitle("ClipSync platform spike")
            .setContentText("Clipboard dataSync service is running")
            .setSmallIcon(android.R.drawable.ic_menu_upload)
            .build()
        startForeground(SpikeContract.NOTIFICATION_ID, notification)
        state().edit()
            .putString(SpikeContract.KEY_SERVICE_RUNNING, "true")
            .putString(SpikeContract.KEY_NOTIFICATION_STATE, SpikeContract.notificationState(this))
            .commit()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            SpikeContract.ACTION_WRITE_CLIPBOARD -> writeClipboard(intent)
            SpikeContract.ACTION_PROBE_BACKGROUND_READ -> probeBackgroundRead()
            SpikeContract.ACTION_HEALTH_CHECK, null -> Unit
        }
        return START_NOT_STICKY
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onDestroy() {
        state().edit().putString(SpikeContract.KEY_SERVICE_RUNNING, "false").commit()
        super.onDestroy()
    }

    private fun writeClipboard(intent: Intent) {
        val text = intent.getStringExtra(SpikeContract.EXTRA_CLIPBOARD_TEXT) ?: return
        getSystemService(ClipboardManager::class.java)
            .setPrimaryClip(ClipData.newPlainText("ClipSync spike", text))
        state().edit().putString(SpikeContract.KEY_LAST_WRITE, text).commit()
    }

    private fun probeBackgroundRead() {
        val result = try {
            if (getSystemService(ClipboardManager::class.java).primaryClip == null) "null" else "value"
        } catch (_: SecurityException) {
            "security_exception"
        }
        state().edit().putString(SpikeContract.KEY_BACKGROUND_READ, result).commit()
    }

    private fun state() = getSharedPreferences(SpikeContract.PREFERENCES, Context.MODE_PRIVATE)
}
