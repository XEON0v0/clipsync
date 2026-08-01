package com.clipsync.app

import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.os.Build
import android.service.quicksettings.TileService

class ClipSyncTileService : TileService() {
    override fun onClick() {
        super.onClick()
        val launchIntent = Intent(this, FocusClipboardActivity::class.java).apply {
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TASK)
        }
        val preferences = getSharedPreferences(SpikeContract.PREFERENCES, Context.MODE_PRIVATE)
        val requestedMode = preferences.getString(SpikeContract.KEY_TILE_MODE, SpikeContract.TILE_MODE_INTENT)

        if (requestedMode == SpikeContract.TILE_MODE_PENDING_INTENT && Build.VERSION.SDK_INT >= 34) {
            val pendingIntent = PendingIntent.getActivity(
                this,
                34,
                launchIntent,
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
            )
            startActivityAndCollapse(pendingIntent)
            preferences.edit()
                .putString(SpikeContract.KEY_TILE_LAUNCH_USED, SpikeContract.TILE_MODE_PENDING_INTENT)
                .commit()
        } else if (Build.VERSION.SDK_INT >= 34) {
            // Apps targeting SDK 34 are no longer allowed to start an activity
            // from a tile with a raw Intent; the platform rejects the call.
            try {
                @Suppress("DEPRECATION")
                startActivityAndCollapse(launchIntent)
                preferences.edit()
                    .putString(SpikeContract.KEY_TILE_LAUNCH_USED, SpikeContract.TILE_MODE_INTENT)
                    .commit()
            } catch (_: UnsupportedOperationException) {
                preferences.edit()
                    .putString(SpikeContract.KEY_TILE_LAUNCH_USED, SpikeContract.TILE_LAUNCH_INTENT_BLOCKED)
                    .commit()
            }
        } else {
            @Suppress("DEPRECATION")
            startActivityAndCollapse(launchIntent)
            preferences.edit()
                .putString(SpikeContract.KEY_TILE_LAUNCH_USED, SpikeContract.TILE_MODE_INTENT)
                .commit()
        }
    }
}
