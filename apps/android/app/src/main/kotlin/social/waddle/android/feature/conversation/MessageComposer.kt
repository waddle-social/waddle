package social.waddle.android.feature.conversation

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.outlined.AttachFile
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import social.waddle.android.R

/** Text input row + send button (send clears the draft optimistically). */
@Composable
fun MessageComposer(
    onSend: (String) -> Unit,
    modifier: Modifier = Modifier,
    onDraftChanged: () -> Unit = {},
    editing: ComposerMode.Editing? = null,
    onCancelEdit: () -> Unit = {},
    replying: ComposerMode.Replying? = null,
    onCancelReply: () -> Unit = {},
    onAttach: (() -> Unit)? = null,
    uploadState: UploadState = UploadState.Idle,
    onClearUpload: () -> Unit = {},
) {
    var draft by rememberSaveable { mutableStateOf("") }

    // Entering edit mode loads the original body; leaving it clears the
    // draft (both edit-sent and cancelled).
    LaunchedEffect(editing) {
        draft = editing?.originalBody.orEmpty()
    }

    Surface(tonalElevation = 3.dp, modifier = modifier.fillMaxWidth()) {
        Column(modifier = Modifier.navigationBarsPadding()) {
            if (editing != null) {
                ComposerBanner(
                    text = stringResource(R.string.composer_editing),
                    cancelContentDescription = stringResource(R.string.composer_cancel_edit),
                    onCancel = onCancelEdit,
                )
            }
            if (replying != null && editing == null) {
                ComposerBanner(
                    text = stringResource(R.string.composer_replying_to, replying.authorName),
                    cancelContentDescription = stringResource(R.string.composer_cancel_reply),
                    onCancel = onCancelReply,
                )
            }
            when (uploadState) {
                UploadState.Idle -> Unit
                UploadState.Uploading -> ComposerBanner(
                    text = stringResource(R.string.upload_in_progress),
                    cancelContentDescription = null,
                    onCancel = null,
                )
                UploadState.TooLarge -> ComposerBanner(
                    text = stringResource(R.string.upload_too_large),
                    cancelContentDescription = stringResource(R.string.upload_dismiss),
                    onCancel = onClearUpload,
                    isError = true,
                )
                UploadState.Failed -> ComposerBanner(
                    text = stringResource(R.string.upload_failed),
                    cancelContentDescription = stringResource(R.string.upload_dismiss),
                    onCancel = onClearUpload,
                    isError = true,
                )
            }
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 8.dp, vertical = 8.dp),
                verticalAlignment = Alignment.Bottom,
            ) {
                if (onAttach != null) {
                    IconButton(
                        onClick = onAttach,
                        enabled = uploadState != UploadState.Uploading,
                        modifier = Modifier
                            .padding(end = 4.dp)
                            .size(48.dp),
                    ) {
                        Icon(
                            Icons.Outlined.AttachFile,
                            contentDescription = stringResource(R.string.composer_attach),
                        )
                    }
                }
                OutlinedTextField(
                    value = draft,
                    onValueChange = { value ->
                        if (value != draft) onDraftChanged()
                        draft = value
                    },
                    modifier = Modifier.weight(1f),
                    placeholder = { Text(text = stringResource(R.string.composer_placeholder)) },
                    maxLines = 5,
                )
                IconButton(
                    onClick = {
                        onSend(draft)
                        draft = ""
                    },
                    enabled = draft.isNotBlank(),
                    modifier = Modifier
                        .padding(start = 4.dp)
                        .size(48.dp),
                ) {
                    Icon(
                        Icons.AutoMirrored.Filled.Send,
                        contentDescription = stringResource(R.string.composer_send),
                    )
                }
            }
        }
    }
}

@Composable
private fun ComposerBanner(
    text: String,
    cancelContentDescription: String?,
    onCancel: (() -> Unit)?,
    isError: Boolean = false,
) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .padding(start = 16.dp, end = 4.dp, top = 4.dp),
    ) {
        Text(
            text = text,
            style = MaterialTheme.typography.labelMedium,
            color = if (isError) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.primary,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
        if (onCancel != null) {
            IconButton(onClick = onCancel) {
                Icon(
                    Icons.Filled.Close,
                    contentDescription = cancelContentDescription,
                )
            }
        }
    }
}
