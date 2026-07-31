package com.clipsync.app

import android.app.Service
import android.content.Intent
import android.os.IBinder

class ClipboardSyncService : Service() {
    override fun onBind(intent: Intent?): IBinder? = null
}
