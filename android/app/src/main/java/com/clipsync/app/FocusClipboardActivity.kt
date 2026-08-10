package com.clipsync.app

import android.content.Context
import android.os.Bundle
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.clipsync.app.ui.theme.ClipSyncTheme

class FocusClipboardActivity : ComponentActivity() {
    private var statusText by mutableStateOf("")
    private var clipboardRead = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        ClipboardSyncService.start(this)
        statusText = getString(R.string.focus_reading)
        setContent {
            ClipSyncTheme {
                Surface(modifier = Modifier.fillMaxSize()) {
                    Box(
                        modifier = Modifier
                            .fillMaxSize()
                            .safeDrawingPadding()
                            .padding(horizontal = 32.dp),
                        contentAlignment = Alignment.Center,
                    ) {
                        Text(
                            text = statusText,
                            style = MaterialTheme.typography.titleMedium,
                            textAlign = TextAlign.Center,
                        )
                    }
                }
            }
        }
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
        statusText = getString(R.string.focus_sending)
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
