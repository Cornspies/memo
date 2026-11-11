package memo.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

private const val SAVE_DELAY = 400L

/** The editor's state. Lives as long as the screen; `finish` runs from the app scope on close. */
private class Editor(val app: AppState, val id: String) {
    var memo by mutableStateOf<MemoDetail?>(null)
    var missing by mutableStateOf(false)
    var editing by mutableStateOf(false)
    var text by mutableStateOf(TextFieldValue(""))
    /** The body last saved or loaded. */
    var saved = ""
    var saveJob: Job? = null

    suspend fun refresh() {
        if (editing) save()
        val next =
            try {
                app.backend.memo(id)
            } catch (e: BackendException) {
                if (e.code == "not_found") missing = true else app.error(e)
                return
            }
        // A sync may have merged another device's edit into the text being edited.
        if (!editing || next.body != saved) adopt(next.body)
        memo = next
        missing = false
        if (next.conflictWith != null || !next.summary.writable) editing = false
    }

    fun adopt(body: String) {
        val cursor = text.selection.start.coerceAtMost(body.length)
        text = TextFieldValue(body, TextRange(cursor))
        saved = body
    }

    fun changed(value: TextFieldValue) {
        val edited = value.text != text.text
        text = value
        if (!edited) return
        saveJob?.cancel()
        saveJob = app.scope.launch {
            delay(SAVE_DELAY)
            save()
        }
    }

    suspend fun save(prune: Boolean = false) {
        saveJob?.cancel()
        val body = text.text
        if (body == saved && !prune) return
        try {
            app.backend.edit(id, body, prune)
            saved = body
        } catch (e: BackendException) {
            if (e.code == "in_conflict") {
                editing = false
                app.notify(Notice(Tone.Warning, "This memo was changed elsewhere at the same time. Choose what to keep."))
            } else {
                app.error(e)
            }
        }
    }

    /** Leaves edit mode: saves, drops unused images, and discards a new memo left empty. */
    suspend fun finish(stay: Boolean) {
        editing = false
        save(prune = true)
        val m = memo
        if (m != null && m.own && text.text.isBlank() && m.serverVersion == 0L && m.attachments.isEmpty()) {
            runCatching { app.backend.remove(id) }
            if (stay) app.back()
            return
        }
        if (stay) refresh()
    }

    fun startEditing() {
        text = text.copy(selection = TextRange(text.text.length))
        editing = true
    }

    fun insert(snippet: String) {
        val value = text
        val start = value.selection.min
        val before = value.text.substring(0, start)
        val prefix = if (before.isNotEmpty() && !before.endsWith("\n")) "\n" else ""
        val body = before + prefix + snippet + value.text.substring(value.selection.max)
        changed(TextFieldValue(body, TextRange(start + prefix.length + snippet.length)))
    }

    /** Inserts a link to `target` at the cursor, as `[title](memo:<id>)`. */
    fun link(target: MemoSummary) {
        val label = target.title.ifEmpty { "Untitled" }.replace(Regex("[\\\\\\[\\]]")) { "\\${it.value}" }
        val snippet = "[$label](memo:${target.id})"
        val value = text
        val body = value.text.substring(0, value.selection.min) + snippet + value.text.substring(value.selection.max)
        changed(TextFieldValue(body, TextRange(value.selection.min + snippet.length)))
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MemoScreen(app: AppState, id: String, startEditing: Boolean) {
    val editor = remember(id) { Editor(app, id).also { it.editing = startEditing } }
    val version by app.backend.version.collectAsState()
    val scope = rememberCoroutineScope()
    var menu by remember { mutableStateOf(false) }
    var dialog by remember { mutableStateOf<String?>(null) }
    var attaching by remember { mutableStateOf(false) }
    val focus = remember { FocusRequester() }

    LaunchedEffect(version) { editor.refresh() }
    DisposableEffect(editor) {
        onDispose { if (editor.editing) app.scope.launch { editor.finish(stay = false) } }
    }
    LaunchedEffect(editor.editing, editor.memo != null) {
        if (editor.editing && editor.memo != null) runCatching { focus.requestFocus() }
    }

    val memo = editor.memo
    val writable = memo != null && memo.summary.writable && memo.conflictWith == null

    fun attach() {
        scope.launch {
            val picked = app.platform.pickImage() ?: return@launch
            attaching = true
            try {
                editor.save()
                val attachment = app.backend.attachImage(id, picked)
                editor.insert("![](attachment:$attachment)\n")
                editor.save()
                editor.refresh()
            } catch (e: BackendException) {
                app.error(e)
            } finally {
                attaching = false
            }
        }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = {},
                navigationIcon = { IconButton({ app.back() }) { Icon(Icons.Back, "Back") } },
                actions = {
                    if (editor.editing) {
                        IconAction(Icons.Image, "Add image", enabled = !attaching) { attach() }
                        IconAction(Icons.Link, "Link memo") { dialog = "link" }
                        IconAction(Icons.Check, "Done") { scope.launch { editor.finish(stay = true) } }
                    } else if (writable) {
                        IconAction(Icons.Edit, "Edit") { editor.startEditing() }
                    }
                    if (memo != null) {
                        IconAction(Icons.More, "More") { menu = true }
                        MemoMenu(app, memo, menu, { menu = false }) { dialog = it }
                    }
                },
            )
        },
    ) { padding ->
        Column(Modifier.padding(padding).fillMaxSize().imePadding()) {
            when {
                editor.missing -> Message("This memo was deleted, or you no longer have access to it.")
                memo == null -> {}
                editor.editing ->
                    BasicTextField(
                        editor.text,
                        editor::changed,
                        textStyle = MaterialTheme.typography.bodyLarge.copy(color = MaterialTheme.colorScheme.onSurface),
                        cursorBrush = SolidColor(MaterialTheme.colorScheme.primary),
                        modifier =
                            Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp)
                                .focusRequester(focus),
                    )
                else ->
                    Column(
                        Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
                        verticalArrangement = Arrangement.spacedBy(12.dp),
                    ) {
                        Notices(app, memo)
                        if (memo.conflictWith != null) {
                            ConflictView(app, memo)
                        } else {
                            val toggle: ((Int) -> Unit)? =
                                if (writable) {
                                    { offset -> app.run { app.backend.toggleTask(id, offset) } }
                                } else {
                                    null
                                }
                            if (memo.body.isBlank()) {
                                Message(if (writable) "Empty. Tap the pencil to write." else "Empty.")
                            }
                            Markdown(RenderContext(app, id, memo.attachments, toggle), memo.body)
                        }
                        if (memo.linkedFrom.isNotEmpty()) {
                            HorizontalDivider()
                            Text("Linked from", style = MaterialTheme.typography.labelLarge)
                            for (from in memo.linkedFrom) {
                                Text(
                                    from.title.ifEmpty { "Untitled" },
                                    color = MaterialTheme.colorScheme.primary,
                                    modifier = Modifier.fillMaxWidth().clickable { app.go(Screen.Memo(from.id)) }.padding(vertical = 4.dp),
                                )
                            }
                        }
                        HorizontalDivider()
                        Footer(app, memo)
                    }
            }
        }
    }

    when (dialog) {
        "link" ->
            LinkDialog(app, id, { dialog = null }) { target ->
                dialog = null
                editor.link(target)
            }
        "share" -> if (memo != null) ShareDialog(app, memo) { dialog = null }
        "history" -> if (memo != null) HistoryDialog(app, memo) { dialog = null }
        "delete" ->
            AlertDialog(
                onDismissRequest = { dialog = null },
                title = { Text("Delete this memo?") },
                text = { Text(if (memo?.summary?.shared == true) "It is deleted for everyone it is shared with." else "It is deleted on all your devices.") },
                confirmButton = {
                    TextButton({
                        dialog = null
                        app.run {
                            editor.editing = false
                            app.backend.remove(id)
                            app.back()
                        }
                    }) { Text("Delete") }
                },
                dismissButton = { TextButton({ dialog = null }) { Text("Cancel") } },
            )
        "leave" ->
            AlertDialog(
                onDismissRequest = { dialog = null },
                title = { Text("Leave this memo?") },
                text = { Text("It disappears from your devices. The owner can share it with you again.") },
                confirmButton = {
                    TextButton({
                        dialog = null
                        app.run {
                            app.backend.leave(id)
                            app.back()
                        }
                    }) { Text("Leave") }
                },
                dismissButton = { TextButton({ dialog = null }) { Text("Cancel") } },
            )
    }
}

@Composable
private fun MemoMenu(app: AppState, memo: MemoDetail, open: Boolean, onDismiss: () -> Unit, onDialog: (String) -> Unit) {
    val s = memo.summary
    DropdownMenu(open, onDismiss) {
        DropdownMenuItem({ Text(if (s.pinned) "Unpin" else "Pin") }, {
            onDismiss()
            app.run { app.backend.setState(memo.id, !s.pinned, s.archived) }
        })
        DropdownMenuItem({ Text(if (s.archived) "Move out of the archive" else "Archive") }, {
            onDismiss()
            app.run {
                app.backend.setState(memo.id, false, !s.archived)
                if (!s.archived) app.back()
            }
        })
        DropdownMenuItem({ Text(if (memo.own) "Share" else "Members") }, {
            onDismiss()
            onDialog("share")
        })
        DropdownMenuItem({ Text("History") }, {
            onDismiss()
            onDialog("history")
        })
        if (s.pushError == "forbidden" || s.undecoded == "newer_format") {
            DropdownMenuItem({ Text("Save as a private copy") }, {
                onDismiss()
                app.run { app.replace(Screen.Memo(app.backend.saveAsCopy(memo.id))) }
            })
        }
        if (memo.own) {
            DropdownMenuItem({ Text("Delete", color = MaterialTheme.colorScheme.error) }, {
                onDismiss()
                onDialog("delete")
            })
        } else {
            DropdownMenuItem({ Text("Leave", color = MaterialTheme.colorScheme.error) }, {
                onDismiss()
                onDialog("leave")
            })
        }
    }
}

/** Picks a memo to link to: search by text, archived memos included. */
@Composable
private fun LinkDialog(app: AppState, exclude: String, onClose: () -> Unit, onPick: (MemoSummary) -> Unit) {
    var query by remember { mutableStateOf("") }
    var results by remember { mutableStateOf<List<MemoSummary>>(emptyList()) }
    LaunchedEffect(query) {
        delay(150)
        try {
            results =
                (app.backend.list(MemoFilter(query = query)) + app.backend.list(MemoFilter(query = query, archived = true)))
                    .filter { it.id != exclude }.take(50)
        } catch (e: BackendException) {
            app.error(e)
        }
    }
    Sheet("Link to a memo", onClose) {
        OutlinedTextField(query, { query = it }, placeholder = { Text("Search") }, singleLine = true, modifier = Modifier.fillMaxWidth())
        if (results.isEmpty()) Text("Nothing matches.", color = MaterialTheme.colorScheme.onSurfaceVariant)
        for (memo in results) {
            Text(
                memo.title.ifEmpty { "Untitled" } + if (memo.archived) " · archived" else "",
                maxLines = 1,
                modifier = Modifier.fillMaxWidth().clickable { onPick(memo) }.padding(vertical = 8.dp),
            )
        }
    }
}

@Composable
fun Message(text: String) {
    Text(text, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(16.dp))
}

@Composable
private fun Notice(text: String, warn: Boolean = true) {
    Surface(
        color = if (warn) MaterialTheme.colorScheme.errorContainer else MaterialTheme.colorScheme.secondaryContainer,
        shape = MaterialTheme.shapes.small,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Text(text, style = MaterialTheme.typography.bodyMedium, modifier = Modifier.padding(12.dp))
    }
}

@Composable
private fun Notices(app: AppState, memo: MemoDetail) {
    val s = memo.summary
    when (s.undecoded) {
        "missing_key" -> Notice("A newer version is waiting for its key. It appears after the next sync.", warn = false)
        "undecryptable" -> Notice("The newest version on the server cannot be decrypted. This is the last good copy.")
        "newer_format" -> Notice("Saved by a newer app version: read-only here. Update the app to edit it.")
    }
    when (s.pushError) {
        "forbidden" -> Notice("You can no longer edit this memo. Your changes are kept on this device; save them as a private copy.")
        "invalid" -> Notice("The server refused this version. Edit the memo to try again.")
        "quota_exceeded" -> Notice("Your storage quota is full. This change stays on this device until you free up space.")
    }
    if (!s.writable && s.pushError == null && s.undecoded == null && !memo.own) {
        Notice("Shared with you to read.", warn = false)
    }
}

@Composable
private fun Footer(app: AppState, memo: MemoDetail) {
    val owner = memo.members.find { it.userId == memo.summary.ownerId }?.username
    val author =
        memo.updatedBy?.takeIf { memo.members.size > 1 }?.let { by ->
            memo.members.find { it.userId == by }?.let { if (it.trust == "me") "you" else it.username }
        }
    val lines =
        buildList {
            add("Edited ${app.platform.dateTime(memo.summary.updatedAt)}" + (author?.let { " by $it" } ?: ""))
            add("Created ${app.platform.dateTime(memo.summary.createdAt)}")
            if (!memo.own && owner != null) add("Owned by $owner")
            if (memo.members.size > 1) add("Shared with ${memo.members.size - 1}")
            if (memo.summary.dirty) add("Not synced yet")
        }
    for (line in lines) Text(line, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
}

@Composable
private fun ConflictView(app: AppState, memo: MemoDetail) {
    val theirs = memo.conflictWith ?: return
    var merging by remember { mutableStateOf(false) }
    var merged by remember { mutableStateOf("") }
    var busy by remember { mutableStateOf(false) }

    fun resolve(body: String) {
        busy = true
        app.scope.launch {
            try {
                app.backend.resolve(memo.id, body)
            } catch (e: BackendException) {
                app.error(e)
            } finally {
                busy = false
            }
        }
    }

    Notice(
        "This memo was changed on this device and elsewhere at the same time, in the same place. Nothing is " +
            "lost: choose which version to keep, or combine them.",
    )
    if (merging) {
        OutlinedTextField(
            merged,
            { merged = it },
            textStyle = MaterialTheme.typography.bodyMedium.copy(fontFamily = FontFamily.Monospace),
            modifier = Modifier.fillMaxWidth().heightIn(min = 240.dp),
        )
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button({ resolve(merged) }, enabled = !busy) { Text("Save combined version") }
            OutlinedButton({ merging = false }) { Text("Back") }
        }
        return
    }
    val ctx = RenderContext(app, memo.id, memo.attachments, null)
    Text("This device", style = MaterialTheme.typography.titleSmall)
    Markdown(ctx, memo.body)
    Button({ resolve(memo.body) }, enabled = !busy) { Text("Keep this version") }
    HorizontalDivider()
    Text("Other version · ${app.platform.relative(theirs.editedAt)}", style = MaterialTheme.typography.titleSmall)
    Markdown(ctx, theirs.body)
    Button({ resolve(theirs.body) }, enabled = !busy) { Text("Keep this version") }
    OutlinedButton({
        merged = "${memo.body.trimEnd()}\n\n---\n\n${theirs.body.trimStart()}"
        merging = true
    }) { Text("Combine both by hand") }
}
