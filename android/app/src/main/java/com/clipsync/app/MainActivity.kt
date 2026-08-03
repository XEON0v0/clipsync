package com.clipsync.app

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.provider.Settings
import androidx.activity.result.contract.ActivityResultContracts
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.Button
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.getValue
import androidx.compose.runtime.collectAsState
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp

class MainActivity : ComponentActivity() {
    private val notificationPermission = registerForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) {
        app.refreshNotificationState()
        ClipboardSyncService.start(this)
    }

    private val app: ClipSyncApp
        get() = application as ClipSyncApp

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            val uiState by app.uiState.collectAsState()
            MaterialTheme {
                Surface(modifier = Modifier.fillMaxSize()) {
                    Column(
                        modifier = Modifier.padding(horizontal = 24.dp, vertical = 28.dp),
                        verticalArrangement = Arrangement.spacedBy(12.dp),
                        horizontalAlignment = Alignment.Start,
                    ) {
                        Text("ClipSync", style = MaterialTheme.typography.headlineMedium)
                        Text("同步状态：${uiState.coreStatus}")
                        val notificationLabel = when (uiState.notificationState) {
                            NotificationState.RESTRICTED -> getString(R.string.notification_restricted)
                            NotificationState.VISIBLE -> getString(R.string.notification_visible)
                        }
                        Text("通知状态：$notificationLabel")
                        uiState.lastError?.let { Text(it, color = MaterialTheme.colorScheme.error) }
                        if (uiState.notificationState == NotificationState.RESTRICTED) {
                            Spacer(Modifier.height(4.dp))
                            Button(onClick = ::openNotificationSettings) {
                                Icon(Icons.Default.Settings, contentDescription = null)
                                Text(getString(R.string.notification_settings))
                            }
                        }
                    }
                }
            }
        }
        requestNotificationPermissionThenStart()
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus) app.refreshNotificationState()
    }

    private fun requestNotificationPermissionThenStart() {
        val needsRequest = Build.VERSION.SDK_INT >= 33 &&
            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED &&
            !app.notificationPermissionWasAsked()
        if (needsRequest) {
            app.markNotificationPermissionAsked()
            notificationPermission.launch(Manifest.permission.POST_NOTIFICATIONS)
        } else {
            ClipboardSyncService.start(this)
        }
    }

    private fun openNotificationSettings() {
        val notificationSettings = Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS).apply {
            putExtra(Settings.EXTRA_APP_PACKAGE, packageName)
        }
        val destination = if (notificationSettings.resolveActivity(packageManager) != null) {
            notificationSettings
        } else {
            Intent(
                Settings.ACTION_APPLICATION_DETAILS_SETTINGS,
                Uri.parse("package:$packageName"),
            )
        }
        startActivity(destination)
    }
}
