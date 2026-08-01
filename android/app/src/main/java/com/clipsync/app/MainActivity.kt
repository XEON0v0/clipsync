package com.clipsync.app

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.Column
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val notificationState = SpikeContract.notificationState(this)
        setContent {
            MaterialTheme {
                Column {
                    Text("ClipSync")
                    Text("Notification visibility: $notificationState")
                }
            }
        }
    }
}
