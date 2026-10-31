package social.waddle.android.flutter

import android.content.ClipboardManager
import android.net.Uri
import android.view.DragAndDropPermissions
import android.view.DragEvent
import io.flutter.embedding.android.FlutterFragmentActivity
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import social.waddle.android.feature.conversation.ConversationViewModel
import social.waddle.android.feature.conversation.UploadState

/** Native content grants stay alive while the encrypted attachment uploader reads the image. */
internal class FlutterContentInsertion(
    private val activity: FlutterFragmentActivity,
    private val scope: CoroutineScope,
    private val currentModel: () -> ConversationViewModel?,
) {
    private var dropRead: Job? = null

    fun insert(uri: Uri, target: ConversationViewModel): Boolean {
        if (target !== currentModel() || uri.scheme != "content") return false
        if (target.uploadState.value == UploadState.Uploading) return false
        val mimeType = runCatching { activity.contentResolver.getType(uri) }.getOrNull()
        if (mimeType?.startsWith("image/") != true) return false
        target.sendAttachment(uri)
        return true
    }

    fun insertBytes(data: ByteArray, mediaType: String, target: ConversationViewModel): Boolean {
        if (target !== currentModel() || !mediaType.startsWith("image/")) return false
        if (target.uploadState.value == UploadState.Uploading || data.isEmpty()) return false
        target.sendAttachmentImageBytes(data, mediaType)
        return true
    }

    fun pasteImage(target: ConversationViewModel): Boolean {
        val clipboard = activity.getSystemService(ClipboardManager::class.java) ?: return false
        val clip = runCatching { clipboard.primaryClip }.getOrNull() ?: return false
        for (index in 0 until clip.itemCount) {
            val uri = clip.getItemAt(index).uri ?: continue
            if (insert(uri, target)) return true
        }
        return false
    }

    fun handleDrop(event: DragEvent): Boolean {
        val target = currentModel() ?: return false
        if (event.clipDescription?.hasMimeType("image/*") != true) return false
        return when (event.action) {
            DragEvent.ACTION_DRAG_STARTED -> target.uploadState.value != UploadState.Uploading
            DragEvent.ACTION_DROP -> acceptDrop(event, target)
            else -> false
        }
    }

    private fun acceptDrop(event: DragEvent, target: ConversationViewModel): Boolean {
        val clip = event.clipData ?: return false
        val permission = activity.requestDragAndDropPermissions(event)
        for (index in 0 until clip.itemCount) {
            val uri = clip.getItemAt(index).uri ?: continue
            if (insert(uri, target)) {
                holdDropPermission(target, permission)
                return true
            }
        }
        permission?.release()
        return false
    }

    private fun holdDropPermission(
        target: ConversationViewModel,
        permission: DragAndDropPermissions?,
    ) {
        dropRead?.cancel()
        dropRead =
            scope.launch {
                try {
                    target.uploadState.first { it != UploadState.Uploading }
                } finally {
                    permission?.release()
                }
            }
    }

    fun close() {
        dropRead?.cancel()
        dropRead = null
    }
}
