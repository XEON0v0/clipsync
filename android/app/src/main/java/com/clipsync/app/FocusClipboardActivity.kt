package com.clipsync.app

import android.app.Activity
import android.content.Context
import android.os.Bundle
import android.widget.Toast
import android.widget.TextView

class FocusClipboardActivity : Activity() {
    private lateinit var statusView: TextView
    private var clipboardRead = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        ClipboardSyncService.start(this)
        statusView = TextView(this).apply {
            text = getString(R.string.focus_reading)
            textSize = 18f
            setPadding(48, 72, 48, 48)
        }
        setContentView(statusView)
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus && !clipboardRead) {
            clipboardRead = true
            getSharedPreferences(AppContract.PREFERENCES, Context.MODE_PRIVATE).edit()
                .putBoolean(AppContract.KEY_WINDOW_FOCUS_READ, true)
                .commit()
            when (val captured = ClipboardCapture.read(this)) {
                is ClipboardCaptureResult.Ready -> send(captured.payload)
                ClipboardCaptureResult.Empty -> finishWith(SendResult.Empty)
                ClipboardCaptureResult.Oversize -> finishWith(SendResult.Oversize)
                ClipboardCaptureResult.Unsupported -> finishWith(SendResult.Unsupported)
            }
        }
    }

    private fun send(payload: ClipboardPayload) {
        statusView.text = getString(R.string.focus_sending)
        (application as ClipSyncApp).send(payload) { result ->
            runOnUiThread { finishWith(result) }
        }
    }

    private fun finishWith(result: SendResult) {
        getSharedPreferences(AppContract.PREFERENCES, Context.MODE_PRIVATE).edit()
            .putString(AppContract.KEY_LAST_SEND_RESULT, result.preferenceValue())
            .commit()
        val message = when (result) {
            is SendResult.Sent -> R.string.send_success
            is SendResult.Failed -> R.string.send_failed
            SendResult.Empty -> R.string.send_empty
            SendResult.Oversize -> R.string.send_oversize
            SendResult.ServiceUnavailable -> R.string.send_service_unavailable
            SendResult.Unsupported -> R.string.send_unsupported
        }
        Toast.makeText(this, message, Toast.LENGTH_LONG).show()
        finish()
    }
}
