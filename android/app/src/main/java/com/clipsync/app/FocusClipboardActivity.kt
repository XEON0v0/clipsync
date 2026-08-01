package com.clipsync.app

import android.app.Activity
import android.content.Context
import android.content.ClipboardManager
import android.os.Bundle
import android.widget.TextView

class FocusClipboardActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(TextView(this).apply { text = "ClipSync clipboard focus probe" })
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus) {
            recordClipboardRead("window_focus")
        }
    }

    private fun recordClipboardRead(trigger: String) {
        val clipboard = getSystemService(ClipboardManager::class.java)
        val text = clipboard.primaryClip
            ?.takeIf { it.itemCount > 0 }
            ?.getItemAt(0)
            ?.coerceToText(this)
            ?.toString()
            ?: "null"
        getSharedPreferences(SpikeContract.PREFERENCES, Context.MODE_PRIVATE).edit()
            .putString(SpikeContract.KEY_FOCUS_READ, text)
            .putString(SpikeContract.KEY_READ_TRIGGER, trigger)
            .putString(SpikeContract.KEY_HAS_WINDOW_FOCUS, hasWindowFocus().toString())
            .commit()
    }
}
