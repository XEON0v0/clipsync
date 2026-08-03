package com.clipsync.app

import android.app.Service
import android.content.Context
import android.content.Intent
import android.os.IBinder
import androidx.core.content.ContextCompat

class ClipboardSyncService : Service() {
    override fun onCreate() {
        super.onCreate()
        startForeground(
            AppContract.SERVICE_NOTIFICATION_ID,
            ClipSyncNotifications.serviceNotification(this),
        )
        state().edit()
            .putBoolean(AppContract.KEY_SERVICE_RUNNING, true)
            .putString(
                AppContract.KEY_NOTIFICATION_STATE,
                AppContract.notificationState(this).persistedValue,
            )
            .commit()
        (application as ClipSyncApp).startCoreAfterForeground()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        (application as ClipSyncApp).startCoreAfterForeground()
        return START_STICKY
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onDestroy() {
        state().edit().putBoolean(AppContract.KEY_SERVICE_RUNNING, false).commit()
        (application as ClipSyncApp).stopCore()
        super.onDestroy()
    }

    private fun state() = getSharedPreferences(AppContract.PREFERENCES, Context.MODE_PRIVATE)

    companion object {
        fun start(context: Context) {
            ContextCompat.startForegroundService(
                context,
                Intent(context, ClipboardSyncService::class.java),
            )
        }
    }
}
