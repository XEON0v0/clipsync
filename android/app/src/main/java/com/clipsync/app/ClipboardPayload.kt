package com.clipsync.app

import android.content.ClipData
import android.content.ClipDescription
import android.content.ClipboardManager
import android.content.Context
import android.graphics.BitmapFactory
import android.net.Uri
import androidx.core.content.FileProvider
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.FileOutputStream
import java.io.InputStream
import java.util.UUID
import uniffi.clipboard_core.FfiClipContent
import uniffi.clipboard_core.FfiClipItem

sealed interface ClipboardPayload {
    data class Text(val text: String) : ClipboardPayload
    data class Image(val bytes: ByteArray) : ClipboardPayload
}

sealed interface ClipboardCaptureResult {
    data class Ready(val payload: ClipboardPayload) : ClipboardCaptureResult
    data object Empty : ClipboardCaptureResult
    data object Oversize : ClipboardCaptureResult
    data object Unsupported : ClipboardCaptureResult
}

sealed interface SendResult {
    data class Sent(val sequence: ULong) : SendResult
    data class Failed(val detail: String) : SendResult
    data object Empty : SendResult
    data object Oversize : SendResult
    data object ServiceUnavailable : SendResult
    data object Unsupported : SendResult

    fun preferenceValue(): String = when (this) {
        is Sent -> "sent:$sequence"
        is Failed -> "failed"
        Empty -> "empty"
        Oversize -> "oversize"
        ServiceUnavailable -> "service_unavailable"
        Unsupported -> "unsupported"
    }
}

object ClipboardCapture {
    fun read(context: Context): ClipboardCaptureResult {
        val clipboard = context.getSystemService(ClipboardManager::class.java)
        val clip = clipboard.primaryClip ?: return ClipboardCaptureResult.Empty
        if (clip.itemCount == 0) return ClipboardCaptureResult.Empty
        val item = clip.getItemAt(0)
        val uri = item.uri
        if (uri != null) {
            return readImageUri(context, clip.description, uri)
        }
        val text = item.text?.toString()?.takeIf { it.isNotEmpty() }
            ?: return ClipboardCaptureResult.Unsupported
        return ClipboardCaptureResult.Ready(ClipboardPayload.Text(text))
    }

    private fun readImageUri(
        context: Context,
        description: ClipDescription,
        uri: Uri,
    ): ClipboardCaptureResult {
        val mimeType = context.contentResolver.getType(uri)
            ?: (0 until description.mimeTypeCount)
                .map(description::getMimeType)
                .firstOrNull { it.startsWith("image/") }
        if (mimeType?.startsWith("image/") != true) return ClipboardCaptureResult.Unsupported
        val input = context.contentResolver.openInputStream(uri)
            ?: return ClipboardCaptureResult.Unsupported
        val bytes = input.use { readAtMost(it, AppContract.MAX_IMAGE_BYTES) }
            ?: return ClipboardCaptureResult.Oversize
        if (ImagePolicy.mimeType(bytes) == null) return ClipboardCaptureResult.Unsupported
        return ClipboardCaptureResult.Ready(ClipboardPayload.Image(bytes))
    }
}

fun readAtMost(input: InputStream, limit: Int): ByteArray? {
    val output = ByteArrayOutputStream(minOf(limit, DEFAULT_BUFFER_SIZE))
    val buffer = ByteArray(DEFAULT_BUFFER_SIZE)
    var total = 0
    while (true) {
        val count = input.read(buffer)
        if (count < 0) break
        total += count
        if (total > limit) return null
        output.write(buffer, 0, count)
    }
    return output.toByteArray()
}

object ImagePolicy {
    fun mimeType(bytes: ByteArray): String? {
        if (bytes.isEmpty() || bytes.size > AppContract.MAX_IMAGE_BYTES) return null
        val options = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeByteArray(bytes, 0, bytes.size, options)
        val width = options.outWidth
        val height = options.outHeight
        if (width <= 0 || height <= 0) return null
        val pixels = width.toLong() * height.toLong()
        if (pixels > AppContract.MAX_IMAGE_PIXELS || pixels * 4L > AppContract.MAX_RGBA_BYTES) {
            return null
        }
        return options.outMimeType?.takeIf { it.startsWith("image/") }
    }
}

object LiveClipboardWriter {
    fun apply(context: Context, item: FfiClipItem) {
        val clipboard = context.getSystemService(ClipboardManager::class.java)
        val clip = when (val content = item.content) {
            is FfiClipContent.Text -> ClipData.newPlainText("ClipSync", content.text)
            is FfiClipContent.Image -> imageClip(context, item.id, content.bytes)
        }
        clipboard.setPrimaryClip(clip)
    }

    private fun imageClip(context: Context, id: String, bytes: ByteArray): ClipData {
        val mimeType = ImagePolicy.mimeType(bytes) ?: error("图片格式或尺寸不受支持")
        val file = ReceivedImageStore.save(context, id, bytes, mimeType)
        val uri = FileProvider.getUriForFile(
            context,
            context.packageName + AppContract.FILE_PROVIDER_AUTHORITY_SUFFIX,
            file,
        )
        return ClipData(
            ClipDescription("ClipSync image", arrayOf(mimeType)),
            ClipData.Item(uri),
        )
    }
}

object ReceivedImageStore {
    private const val MAX_FILES = 10
    private const val MAX_AGE_MS = 24L * 60L * 60L * 1000L

    fun save(context: Context, id: String, bytes: ByteArray, mimeType: String): File {
        val safeId = UUID.fromString(id).toString()
        val directory = File(context.filesDir, "received_images").apply { mkdirs() }
        cleanup(directory)
        val extension = when (mimeType) {
            "image/jpeg" -> "jpg"
            "image/webp" -> "webp"
            "image/gif" -> "gif"
            else -> "png"
        }
        val destination = File(directory, "$safeId.$extension")
        val temporary = File(directory, ".$safeId.tmp")
        FileOutputStream(temporary).use { output ->
            output.write(bytes)
            output.fd.sync()
        }
        if (destination.exists() && !destination.delete()) error("无法替换接收图片")
        if (!temporary.renameTo(destination)) {
            temporary.delete()
            error("无法保存接收图片")
        }
        cleanup(directory)
        return destination
    }

    private fun cleanup(directory: File) {
        val now = System.currentTimeMillis()
        directory.listFiles()
            ?.filter { it.isFile && !it.name.startsWith(".") }
            ?.sortedByDescending(File::lastModified)
            ?.forEachIndexed { index, file ->
                if (index >= MAX_FILES || now - file.lastModified() > MAX_AGE_MS) {
                    file.delete()
                }
            }
    }
}
