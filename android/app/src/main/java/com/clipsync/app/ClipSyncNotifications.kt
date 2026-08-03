package com.clipsync.app

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent

object ClipSyncNotifications {
    fun createChannels(context: Context) {
        val manager = context.getSystemService(NotificationManager::class.java)
        manager.createNotificationChannels(
            listOf(
                NotificationChannel(
                    AppContract.SERVICE_CHANNEL,
                    context.getString(R.string.service_channel_name),
                    NotificationManager.IMPORTANCE_LOW,
                ),
                NotificationChannel(
                    AppContract.MAILBOX_CHANNEL,
                    context.getString(R.string.mailbox_channel_name),
                    NotificationManager.IMPORTANCE_HIGH,
                ),
            ),
        )
    }

    fun serviceNotification(context: Context): Notification {
        createChannels(context)
        return Notification.Builder(context, AppContract.SERVICE_CHANNEL)
            .setContentTitle(context.getString(R.string.service_notification_title))
            .setContentText(context.getString(R.string.service_notification_text))
            .setContentIntent(openAppIntent(context))
            .setSmallIcon(android.R.drawable.ic_menu_upload)
            .setOngoing(true)
            .setCategory(Notification.CATEGORY_SERVICE)
            .build()
    }

    fun showMailbox(context: Context) {
        if (AppContract.notificationState(context) != NotificationState.VISIBLE) return
        createChannels(context)
        val notification = Notification.Builder(context, AppContract.MAILBOX_CHANNEL)
            .setContentTitle(context.getString(R.string.mailbox_notification_title))
            .setContentText(context.getString(R.string.mailbox_notification_text))
            .setContentIntent(openAppIntent(context))
            .setSmallIcon(android.R.drawable.ic_dialog_email)
            .setAutoCancel(true)
            .setCategory(Notification.CATEGORY_MESSAGE)
            .build()
        context.getSystemService(NotificationManager::class.java)
            .notify(AppContract.MAILBOX_NOTIFICATION_ID, notification)
    }

    private fun openAppIntent(context: Context): PendingIntent {
        val intent = Intent(context, MainActivity::class.java).apply {
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP)
        }
        return PendingIntent.getActivity(
            context,
            0,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }
}
