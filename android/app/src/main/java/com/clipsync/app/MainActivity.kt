package com.clipsync.app

import android.Manifest
import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.PowerManager
import android.provider.Settings
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Image
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Tab
import androidx.compose.material3.TabRow
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.clipsync.app.ui.theme.ClipSyncTheme
import java.text.DateFormat
import java.util.Date

class MainActivity : ComponentActivity() {
    private var batteryStateRevision by mutableIntStateOf(0)
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
        enableEdgeToEdge()
        setContent {
            val uiState by app.uiState.collectAsState()
            ClipSyncTheme {
                Surface(modifier = Modifier.fillMaxSize()) {
                    ClipSyncScreen(
                        state = uiState,
                        app = app,
                        openNotificationSettings = ::openNotificationSettings,
                        openCameraSettings = ::openAppSettings,
                        sendCurrentClipboard = ::sendCurrentClipboard,
                        requestBatteryExemption = ::requestBatteryExemption,
                        isBatteryExempt = batteryStateRevision.let { isBatteryExempt() },
                    )
                }
            }
        }
        requestNotificationPermissionThenStart()
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus) app.refreshNotificationState()
    }

    override fun onResume() {
        super.onResume()
        batteryStateRevision += 1
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
        val intent = Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS).apply {
            putExtra(Settings.EXTRA_APP_PACKAGE, packageName)
        }
        try {
            startActivity(intent)
        } catch (_: ActivityNotFoundException) {
            startActivity(appDetailsIntent())
        }
    }

    private fun openAppSettings() = startActivity(appDetailsIntent())

    private fun sendCurrentClipboard() {
        startActivity(Intent(this, FocusClipboardActivity::class.java))
    }

    private fun appDetailsIntent() = Intent(
        Settings.ACTION_APPLICATION_DETAILS_SETTINGS,
        Uri.parse("package:$packageName"),
    )

    private fun isBatteryExempt(): Boolean =
        getSystemService(PowerManager::class.java).isIgnoringBatteryOptimizations(packageName)

    private fun requestBatteryExemption() {
        val request = Intent(
            Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS,
            Uri.parse("package:$packageName"),
        )
        try {
            startActivity(request)
        } catch (_: ActivityNotFoundException) {
            startActivity(appDetailsIntent())
        }
    }
}

private enum class AppTab(val label: String) {
    PAIRING("配对"),
    HISTORY("历史"),
    POWER("后台"),
}

@Composable
private fun ClipSyncScreen(
    state: ClipSyncUiState,
    app: ClipSyncApp,
    openNotificationSettings: () -> Unit,
    openCameraSettings: () -> Unit,
    sendCurrentClipboard: () -> Unit,
    requestBatteryExemption: () -> Unit,
    isBatteryExempt: Boolean,
) {
    var selectedTab by remember { mutableStateOf(AppTab.PAIRING) }
    Scaffold(modifier = Modifier.fillMaxSize()) { innerPadding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(innerPadding),
        ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 20.dp, vertical = 16.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            Text("ClipSync", style = MaterialTheme.typography.headlineSmall)
            Text("同步状态：${state.coreStatus}", style = MaterialTheme.typography.bodyMedium)
            val notificationLabel = when (state.notificationState) {
                NotificationState.RESTRICTED -> stringResource(R.string.notification_restricted)
                NotificationState.VISIBLE -> stringResource(R.string.notification_visible)
            }
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    "通知状态：$notificationLabel",
                    color = if (state.notificationState == NotificationState.RESTRICTED) {
                        MaterialTheme.colorScheme.error
                    } else {
                        MaterialTheme.colorScheme.onSurface
                    },
                )
                if (state.notificationState == NotificationState.RESTRICTED) {
                    TextButton(onClick = openNotificationSettings) {
                        Icon(Icons.Default.Settings, contentDescription = null)
                        Text(stringResource(R.string.notification_settings))
                    }
                }
            }
            state.lastError?.let { Text(it, color = MaterialTheme.colorScheme.error) }
        }
        TabRow(selectedTabIndex = selectedTab.ordinal) {
            AppTab.entries.forEach { tab ->
                Tab(
                    selected = selectedTab == tab,
                    onClick = {
                        selectedTab = tab
                        if (tab == AppTab.HISTORY) app.refreshHistory()
                    },
                    text = { Text(tab.label) },
                )
            }
        }
        when (selectedTab) {
            AppTab.PAIRING -> PairingPane(
                pairing = state.pairing,
                app = app,
                openCameraSettings = openCameraSettings,
                sendCurrentClipboard = sendCurrentClipboard,
            )
            AppTab.HISTORY -> HistoryPane(state, app)
            AppTab.POWER -> PowerPane(isBatteryExempt, requestBatteryExemption)
        }
    }
    }
}

@Composable
private fun PairingPane(
    pairing: PairingUiState,
    app: ClipSyncApp,
    openCameraSettings: () -> Unit,
    sendCurrentClipboard: () -> Unit,
) {
    var scanning by remember { mutableStateOf(false) }
    var cameraGranted by remember {
        mutableStateOf(app.checkSelfPermission(Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED)
    }
    val cameraPermission = androidx.activity.compose.rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { granted ->
        cameraGranted = granted
        scanning = granted
        if (!granted) app.reportUiError(app.getString(R.string.camera_permission_required))
    }

    if (scanning && cameraGranted && pairing == PairingUiState.Unpaired) {
        Box(modifier = Modifier.fillMaxSize()) {
            QrScannerView(
                onQrCode = { value ->
                    scanning = false
                    app.claimPairing(value)
                },
                onError = { message ->
                    scanning = false
                    app.reportUiError("扫码失败：$message")
                },
            )
            OutlinedButton(
                onClick = { scanning = false },
                modifier = Modifier
                    .align(Alignment.BottomCenter)
                    .navigationBarsPadding()
                    .padding(24.dp),
            ) {
                Text("取消扫码")
            }
        }
        return
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(24.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        when (pairing) {
            PairingUiState.Unpaired -> {
                Text("尚未配对", style = MaterialTheme.typography.titleLarge)
                Button(
                    onClick = {
                        app.clearUiError()
                        if (cameraGranted) scanning = true else cameraPermission.launch(Manifest.permission.CAMERA)
                    },
                ) {
                    Text("扫描 Mac 配对码")
                }
                if (!cameraGranted) {
                    TextButton(onClick = openCameraSettings) {
                        Text(stringResource(R.string.camera_permission_settings))
                    }
                }
            }
            PairingUiState.Claiming -> {
                CircularProgressIndicator()
                Text("正在验证配对码")
                OutlinedButton(onClick = app::cancelPairing) { Text("取消") }
            }
            is PairingUiState.SasReady -> {
                Text("核对安全码", style = MaterialTheme.typography.titleLarge)
                Text(
                    pairing.sas,
                    style = MaterialTheme.typography.displayMedium,
                    fontWeight = FontWeight.Bold,
                )
                Text("确认此六位数字与 Mac 上显示的完全一致。")
                Button(onClick = app::confirmPairing, enabled = pairing.canConfirm) {
                    Text("数字一致，完成配对")
                }
                OutlinedButton(onClick = app::cancelPairing) { Text("取消配对") }
            }
            is PairingUiState.Paired -> {
                Text("已配对", style = MaterialTheme.typography.titleLarge)
                Text("房间 ${pairing.roomId.take(12)}")
                Button(onClick = sendCurrentClipboard) {
                    Text(stringResource(R.string.send_current_clipboard))
                }
            }
        }
    }
}

@Composable
private fun HistoryPane(state: ClipSyncUiState, app: ClipSyncApp) {
    var confirmClear by remember { mutableStateOf(false) }
    Column(modifier = Modifier.fillMaxSize()) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 12.dp, vertical = 6.dp),
            horizontalArrangement = Arrangement.End,
        ) {
            IconButton(onClick = app::refreshHistory) {
                Icon(Icons.Default.Refresh, contentDescription = "刷新历史")
            }
            IconButton(onClick = { confirmClear = true }, enabled = state.history.isNotEmpty()) {
                Icon(Icons.Default.Delete, contentDescription = "清空历史")
            }
        }
        if (state.historyLoading && state.history.isEmpty()) {
            Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                CircularProgressIndicator()
            }
        } else if (state.history.isEmpty()) {
            Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                Text("暂无历史")
            }
        } else {
            LazyColumn(
                modifier = Modifier.fillMaxSize(),
                contentPadding = PaddingValues(horizontal = 16.dp, vertical = 8.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                items(state.history, key = CoreHistoryItem::id) { item ->
                    HistoryRow(item, app)
                }
            }
        }
    }
    if (confirmClear) {
        AlertDialog(
            onDismissRequest = { confirmClear = false },
            title = { Text("清空历史？") },
            text = { Text("本机保存的 50 条以内历史和图片缓存将被删除。") },
            confirmButton = {
                TextButton(onClick = {
                    confirmClear = false
                    app.clearHistory()
                }) { Text("清空") }
            },
            dismissButton = {
                TextButton(onClick = { confirmClear = false }) { Text("取消") }
            },
        )
    }
}

@Composable
private fun HistoryRow(item: CoreHistoryItem, app: ClipSyncApp) {
    val image = item.content as? CoreHistoryContent.Image
    LaunchedEffect(item.id, image?.bytes) {
        if (image != null && image.bytes == null) app.loadHistoryImage(item.id)
    }
    Card(modifier = Modifier.fillMaxWidth()) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .clickable { app.applyHistory(item.id) }
                .padding(horizontal = 20.dp, vertical = 12.dp),
            horizontalArrangement = Arrangement.spacedBy(14.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            when (val content = item.content) {
                is CoreHistoryContent.Text -> Text(
                    content.text,
                    modifier = Modifier.weight(1f),
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
                is CoreHistoryContent.Image -> {
                    val bitmap = remember(content.bytes) {
                        content.bytes?.let(::decodeHistoryThumbnail)
                    }
                    if (bitmap == null) {
                        Box(Modifier.size(64.dp), contentAlignment = Alignment.Center) {
                            CircularProgressIndicator(modifier = Modifier.size(24.dp))
                        }
                    } else {
                        Image(
                            bitmap = bitmap.asImageBitmap(),
                            contentDescription = "历史图片",
                            modifier = Modifier
                                .size(64.dp)
                                .clip(MaterialTheme.shapes.medium),
                        )
                    }
                    Spacer(Modifier.weight(1f))
                }
            }
            Column(horizontalAlignment = Alignment.End) {
                Text(item.sourceLabel, style = MaterialTheme.typography.labelMedium)
                Text(
                    DateFormat.getDateTimeInstance(DateFormat.SHORT, DateFormat.SHORT)
                        .format(Date(item.tsMs)),
                    style = MaterialTheme.typography.labelSmall,
                )
            }
        }
    }
}

@Composable
private fun PowerPane(isBatteryExempt: Boolean, requestBatteryExemption: () -> Unit) {
    val context = LocalContext.current
    var saveToGallery by remember { mutableStateOf(AppContract.saveToGalleryEnabled(context)) }
    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(24.dp),
        verticalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        Text("后台接收", style = MaterialTheme.typography.titleLarge)
        Text(if (isBatteryExempt) "电池优化白名单：已允许" else "电池优化白名单：未允许")
        if (!isBatteryExempt) {
            Button(onClick = requestBatteryExemption) { Text("允许后台持续接收") }
        }
        HorizontalDivider()
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Column(Modifier.weight(1f)) {
                Text("保存同步图片到图库", style = MaterialTheme.typography.titleMedium)
                Text(
                    "Mac 同步来的图片自动存入 Pictures/ClipSync",
                    style = MaterialTheme.typography.bodySmall,
                )
            }
            Switch(
                checked = saveToGallery,
                onCheckedChange = { checked ->
                    saveToGallery = checked
                    context.getSharedPreferences(AppContract.PREFERENCES, Context.MODE_PRIVATE)
                        .edit()
                        .putBoolean(AppContract.KEY_SAVE_TO_GALLERY, checked)
                        .apply()
                },
            )
        }
        HorizontalDivider()
        Text("国产 ROM 自启动", style = MaterialTheme.typography.titleMedium)
        Text("小米/红米：手机管家 → 应用管理 → 权限 → 自启动管理")
        Text("华为/荣耀：手机管家 → 应用启动管理 → ClipSync → 手动管理")
        Text("OPPO/一加/realme：设置 → 应用 → 自启动 → ClipSync")
        Text("vivo/iQOO：设置 → 应用与权限 → 权限管理 → 自启动")
    }
}
