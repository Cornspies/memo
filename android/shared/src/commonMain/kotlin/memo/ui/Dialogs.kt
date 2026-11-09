package memo.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import kotlinx.coroutines.launch

/** A titled dialog that scrolls and leaves room for the keyboard. */
@Composable
fun Sheet(title: String, onClose: () -> Unit, content: @Composable () -> Unit) {
    Dialog(onClose, DialogProperties(usePlatformDefaultWidth = false)) {
        Surface(
            shape = MaterialTheme.shapes.large,
            modifier = Modifier.widthIn(max = 560.dp).fillMaxWidth().padding(16.dp).imePadding(),
        ) {
            Column(Modifier.verticalScroll(rememberScrollState()).padding(20.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(title, style = MaterialTheme.typography.titleLarge, modifier = Modifier.weight(1f))
                    IconAction(Icons.Close, "Close", onClick = onClose)
                }
                content()
            }
        }
    }
}

@Composable
fun TrustBadge(trust: String) {
    when (trust) {
        "verified" -> Badge("Verified")
        "pinned" -> Badge("Not verified")
        "not_pinned" -> Badge("New contact")
        "changed" -> Badge("Key changed!", warn = true)
    }
}

@Composable
private fun Small(text: String, muted: Boolean = true) {
    Text(
        text,
        style = MaterialTheme.typography.bodySmall,
        color = if (muted) MaterialTheme.colorScheme.onSurfaceVariant else MaterialTheme.colorScheme.onSurface,
    )
}

@Composable
private fun PermissionChoice(permission: String, enabled: Boolean = true, onChange: (String) -> Unit) {
    Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        FilterChip(permission == "read", { onChange("read") }, { Text("can read") }, enabled = enabled)
        FilterChip(permission == "write", { onChange("write") }, { Text("can edit") }, enabled = enabled)
    }
}

@Composable
fun ShareDialog(app: AppState, memo: MemoDetail, onClose: () -> Unit) {
    val session by app.backend.session.collectAsState()
    var username by remember { mutableStateOf("") }
    var found by remember { mutableStateOf<Contact?>(null) }
    var permission by remember { mutableStateOf("read") }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    var verifying by remember { mutableStateOf<MemberView?>(null) }
    var removing by remember { mutableStateOf<MemberView?>(null) }

    fun run(overrides: Map<String, String> = emptyMap(), action: suspend () -> Unit) {
        busy = true
        error = null
        app.scope.launch {
            try {
                action()
            } catch (e: BackendException) {
                error = errorText(e, overrides)
            } finally {
                busy = false
            }
        }
    }

    Sheet(if (memo.own) "Share" else "Members", onClose) {
        error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
        val v = verifying
        val r = removing
        if (v != null) {
            Text("Ask ${v.username} to open Settings and read you their key fingerprint, in person or over a channel you trust. It must match:")
            Text(v.fingerprint, fontFamily = FontFamily.Monospace)
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button({
                    run {
                        app.backend.verifyContact(v.userId)
                        verifying = null
                    }
                }, enabled = !busy) { Text("It matches") }
                TextButton({ verifying = null }) { Text("Cancel") }
            }
            return@Sheet
        }
        if (r != null) {
            Text("Remove ${r.username}? They keep what they already saw, but not later changes.")
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button({
                    run {
                        app.backend.revoke(memo.id, listOf(r.userId))
                        removing = null
                    }
                }, enabled = !busy) { Text("Remove") }
                TextButton({ removing = null }) { Text("Cancel") }
            }
            return@Sheet
        }

        for (member in memo.members) {
            Column(Modifier.fillMaxWidth().padding(vertical = 4.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(member.username, fontWeight = FontWeight.SemiBold)
                    if (member.userId == memo.summary.ownerId) Badge("Owner")
                    if (member.trust == "me") Badge("You")
                    TrustBadge(member.trust)
                }
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                    when {
                        member.userId == memo.summary.ownerId -> Small("can edit")
                        memo.own -> {
                            PermissionChoice(member.permission, enabled = !busy) { next ->
                                if (next != member.permission) run { app.backend.setPermission(memo.id, member.userId, next) }
                            }
                            TextButton({ removing = member }, enabled = !busy) {
                                Text("Remove", color = MaterialTheme.colorScheme.error)
                            }
                        }
                        else -> Small(if (member.permission == "write") "can edit" else "can read")
                    }
                    if (member.trust == "pinned" || member.trust == "not_pinned") {
                        TextButton({ verifying = member }) { Text("Verify") }
                    }
                }
                if (member.trust == "changed") {
                    Small(
                        "The server now presents a different key for ${member.username} than the one you saw before. " +
                            "This can mean an attack. Memos are only encrypted to the key you saw first. Ask them " +
                            "whether they reset their account.",
                        muted = false,
                    )
                }
            }
            HorizontalDivider()
        }

        if (memo.own) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedTextField(
                    username,
                    { username = it },
                    label = { Text("Share with") },
                    placeholder = { Text("Username") },
                    singleLine = true,
                    keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.None, autoCorrectEnabled = false),
                    modifier = Modifier.weight(1f),
                )
                Button({
                    found = null
                    run(mapOf("not_found" to "There is no user with this name.")) { found = app.backend.lookupUser(username) }
                }, enabled = !busy && username.isNotBlank()) { Text("Find") }
            }
            found?.let { user ->
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(user.username, fontWeight = FontWeight.SemiBold)
                    TrustBadge(user.trust)
                }
                when {
                    user.trust == "me" -> Small("That is you.")
                    memo.members.any { it.userId == user.userId } -> Small("Already a member.")
                    user.trust == "changed" ->
                        Small(
                            "The server presents a different key for ${user.username} than the one you saw before. " +
                                "Sharing is blocked until you check with them.",
                            muted = false,
                        )
                    else -> {
                        Small("Key fingerprint:", muted = false)
                        Text(user.fingerprint, fontFamily = FontFamily.Monospace, style = MaterialTheme.typography.bodySmall)
                        if (user.trust == "not_pinned") {
                            Small(
                                "First time you share with them: this key will be remembered. Compare the fingerprint " +
                                    "with them to be sure it is theirs.",
                            )
                        }
                        PermissionChoice(permission) { permission = it }
                        Button({
                            run {
                                app.backend.share(memo.id, user, permission)
                                app.info("Shared with ${user.username}.")
                                found = null
                                username = ""
                            }
                        }, enabled = !busy) { Text("Share") }
                    }
                }
            }
        } else {
            Small("Only the owner can add or remove members.")
        }
        (session as? Session.Unlocked)?.let {
            Small("Your key fingerprint:")
            Text(it.fingerprint, fontFamily = FontFamily.Monospace, style = MaterialTheme.typography.bodySmall)
        }
    }
}

@Composable
fun HistoryDialog(app: AppState, memo: MemoDetail, onClose: () -> Unit) {
    var revisions by remember { mutableStateOf<List<RevisionInfo>?>(null) }
    var selected by remember { mutableStateOf<RevisionInfo?>(null) }
    var body by remember { mutableStateOf<String?>(null) }
    var error by remember { mutableStateOf<String?>(null) }
    var busy by remember { mutableStateOf(false) }

    LaunchedEffect(memo.id) {
        try {
            revisions = app.backend.revisions(memo.id)
        } catch (e: BackendException) {
            error = errorText(e)
        }
    }
    LaunchedEffect(selected) {
        val s = selected ?: return@LaunchedEffect
        body = null
        error = null
        try {
            body = app.backend.revision(memo.id, s.version)
        } catch (e: BackendException) {
            error = errorText(e, mapOf("decrypt" to "This version cannot be decrypted on this device."))
        }
    }

    fun who(userId: String?): String {
        val member = memo.members.find { it.userId == userId }
        return if (member == null) "a former member" else if (member.trust == "me") "you" else member.username
    }

    Sheet("History", onClose) {
        error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
        val s = selected
        if (s != null) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                TextButton({ selected = null }) { Text("← All versions") }
                Small("Version ${s.version} · ${app.platform.dateTime(s.updatedAt)}")
            }
            val b = body
            if (b != null) {
                Markdown(RenderContext(app, memo.id, memo.attachments, null), b, Modifier.heightIn(min = 40.dp))
                if (memo.summary.writable && memo.conflictWith == null && s.version != memo.serverVersion) {
                    Button({
                        busy = true
                        app.scope.launch {
                            try {
                                app.backend.restoreRevision(memo.id, s.version)
                                onClose()
                            } catch (e: BackendException) {
                                error = errorText(e)
                            } finally {
                                busy = false
                            }
                        }
                    }, enabled = !busy) { Text("Restore this version") }
                }
            } else if (error == null) {
                Small("Decrypting…")
            }
            return@Sheet
        }
        val list = revisions
        when {
            list == null -> if (error == null) Small("Loading…")
            list.isEmpty() -> Small("No versions on the server yet. They appear after the first sync.")
            else ->
                for (revision in list) {
                    Column(Modifier.fillMaxWidth().clickable { selected = revision }.padding(vertical = 8.dp)) {
                        Text("Version ${revision.version}" + if (revision.version == memo.serverVersion) " (current)" else "")
                        Small("${app.platform.dateTime(revision.updatedAt)} · by ${who(revision.updatedBy)}")
                    }
                    HorizontalDivider()
                }
        }
    }
}
