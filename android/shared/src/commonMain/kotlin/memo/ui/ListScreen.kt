package memo.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.FloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.delay

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ListScreen(app: AppState) {
    val version by app.backend.version.collectAsState()
    val filter = app.filter
    val platform = app.platform
    var memos by remember { mutableStateOf<List<MemoSummary>?>(null) }
    var tags by remember { mutableStateOf<List<TagCount>>(emptyList()) }
    var saved by remember { mutableStateOf<List<SavedFilter>>(emptyList()) }
    var counts by remember { mutableStateOf<Map<Long, Int>>(emptyMap()) }
    var searching by remember { mutableStateOf(filter.query.isNotEmpty()) }
    var calendar by remember { mutableStateOf(filter.day != null) }
    var menu by remember { mutableStateOf(false) }
    var saving by remember { mutableStateOf<String?>(null) }
    val chips = rememberLazyListState()

    LaunchedEffect(version, filter) {
        if (filter.query.isNotEmpty()) delay(150)
        try {
            memos = app.backend.list(filter)
            tags = app.backend.tags()
            saved = app.backend.savedFilters()
        } catch (e: BackendException) {
            app.error(e)
        }
    }
    LaunchedEffect(version, calendar) {
        if (!calendar) return@LaunchedEffect
        try {
            counts = app.backend.activity().groupingBy { platform.dayStart(it) }.eachCount()
        } catch (e: BackendException) {
            app.error(e)
        }
    }

    fun create() =
        app.run {
            val id = app.backend.create("")
            app.go(Screen.Memo(id, startEditing = true))
        }

    val current = saved.find { filter.isSaved(it) }
    val describe =
        listOfNotNull(
            "Archive".takeIf { filter.archived },
            filter.query.trim().takeIf { it.isNotEmpty() }?.let { "“$it”" },
            filter.tag?.let { "#$it" },
            filter.day?.let { platform.date(it) },
        ).plus(QuickFilters.filter { it.first in filter.has }.map { it.second.lowercase() }).joinToString(" · ")

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Column {
                        Text(if (filter.archived) "Archive" else "memo")
                        SyncLine(app)
                    }
                },
                actions = {
                    IconAction(Icons.Search, "Search") {
                        searching = !searching
                        if (!searching) app.filter = filter.copy(query = "")
                    }
                    IconAction(Icons.Calendar, "Calendar") {
                        calendar = !calendar
                        if (!calendar) app.filter = filter.copy(day = null)
                    }
                    IconAction(Icons.More, "More") { menu = true }
                    DropdownMenu(menu, { menu = false }) {
                        DropdownMenuItem({ Text(if (filter.archived) "Memos" else "Archive") }, {
                            menu = false
                            app.filter = MemoFilter(archived = !filter.archived)
                        })
                        DropdownMenuItem({ Text("Sync now") }, {
                            menu = false
                            app.run { app.backend.sync() }
                        })
                        DropdownMenuItem({ Text("Settings") }, {
                            menu = false
                            app.go(Screen.Settings)
                        })
                    }
                },
            )
        },
        floatingActionButton = {
            if (!filter.archived) {
                FloatingActionButton(::create) { Icon(Icons.Add, "New memo") }
            }
        },
    ) { padding ->
        Column(Modifier.padding(padding).fillMaxSize()) {
            if (searching) {
                OutlinedTextField(
                    filter.query,
                    { app.filter = filter.copy(query = it) },
                    placeholder = { Text("Search") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
                )
            }
            if (calendar) {
                MonthCalendar(platform, counts, filter.day) { day -> app.filter = filter.copy(day = day, archived = false) }
            }
            // Saved filters come first: show a new one.
            LaunchedEffect(saved.size) { chips.scrollToItem(0) }
            LazyRow(
                state = chips,
                contentPadding = PaddingValues(horizontal = 16.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                items(saved, key = { "saved:${it.id}" }) { item ->
                    FilterChip(
                        selected = filter.isSaved(item),
                        onClick = { app.filter = if (filter.isSaved(item)) MemoFilter() else item.toFilter() },
                        label = { Text("★ ${item.name}") },
                    )
                }
                items(QuickFilters, key = { "has:${it.first}" }) { (has, name) ->
                    FilterChip(
                        selected = has in filter.has,
                        onClick = { app.filter = filter.copy(has = if (has in filter.has) filter.has - has else filter.has + has) },
                        label = { Text(name) },
                    )
                }
            }
            if (tags.isNotEmpty() && !filter.archived) {
                LazyRow(
                    contentPadding = PaddingValues(horizontal = 16.dp),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    items(tags, key = { it.tag }) { t ->
                        FilterChip(
                            selected = filter.tag == t.tag,
                            onClick = { app.filter = filter.copy(tag = if (filter.tag == t.tag) null else t.tag) },
                            label = { Text("#${t.tag} ${t.count}") },
                        )
                    }
                }
            }
            if (filter.narrowed) {
                Row(Modifier.fillMaxWidth().padding(start = 16.dp, end = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        describe,
                        style = MaterialTheme.typography.labelLarge,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f),
                    )
                    if (current != null) {
                        SmallButton("Delete") { app.run { app.backend.deleteFilter(current.id) } }
                    } else if (!filter.archived && filter.day == null && (filter.query.isNotBlank() || filter.tag != null || filter.has.isNotEmpty())) {
                        SmallButton("Save") { saving = filter.tag?.let { "#$it" } ?: filter.query.trim() }
                    }
                    SmallButton("Clear") {
                        searching = false
                        app.filter = MemoFilter()
                    }
                }
            }
            val list = memos
            when {
                list == null -> {}
                list.isEmpty() ->
                    Text(
                        when {
                            filter.narrowed && !filter.archived -> "No memos match."
                            filter.archived -> "Nothing archived."
                            else -> "No memos yet. Tap + to write one."
                        },
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(24.dp),
                    )
                else ->
                    LazyColumn(
                        contentPadding = PaddingValues(start = 12.dp, end = 12.dp, top = 8.dp, bottom = 96.dp),
                        verticalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        // The timeline: pinned memos, then one group per day of the last change.
                        var group: Long? = null
                        list.forEachIndexed { i, memo ->
                            val key = if (memo.pinned) -1L else platform.dayStart(memo.updatedAt)
                            if (i == 0 || key != group) {
                                group = key
                                item(key = "day:$key") {
                                    Text(
                                        if (key == -1L) "Pinned" else platform.dayLabel(key),
                                        style = MaterialTheme.typography.labelLarge,
                                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                                        modifier = Modifier.padding(start = 4.dp, top = if (i == 0) 0.dp else 8.dp),
                                    )
                                }
                            }
                            item(key = memo.id) { MemoCard(app, memo) }
                        }
                    }
            }
        }
    }

    saving?.let { name ->
        AlertDialog(
            onDismissRequest = { saving = null },
            title = { Text("Save filter") },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(describe, style = MaterialTheme.typography.bodySmall)
                    OutlinedTextField(name, { saving = it }, label = { Text("Name") }, singleLine = true)
                }
            },
            confirmButton = {
                TextButton({
                    val chosen = name.trim()
                    saving = null
                    app.run { app.backend.saveFilter(chosen, filter) }
                }, enabled = name.isNotBlank()) { Text("Save") }
            },
            dismissButton = { TextButton({ saving = null }) { Text("Cancel") } },
        )
    }
}

@Composable
fun SyncLine(app: AppState) {
    val s by app.backend.sync.collectAsState()
    var tick by remember { mutableStateOf(0L) }
    LaunchedEffect(Unit) {
        while (true) {
            delay(30_000)
            tick++
        }
    }
    val text =
        when {
            s.syncing -> "Syncing…"
            s.error != null -> if (s.unsynced) "Offline: changes are kept here" else "Offline"
            s.unsynced -> "Changes not synced yet"
            s.last != null -> "Synced ${tick.let { app.platform.relative(s.last!!) }}"
            else -> "Not synced yet"
        }
    Text(
        text,
        style = MaterialTheme.typography.labelSmall,
        color = if (s.error != null) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

@Composable
private fun MemoCard(app: AppState, memo: MemoSummary) {
    Card(
        Modifier.fillMaxWidth().clickable { app.go(Screen.Memo(memo.id)) },
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surfaceContainerLow),
    ) {
        Column(Modifier.padding(14.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    memo.title.ifEmpty { if (memo.undecoded != null) "Cannot be shown" else "Untitled" },
                    style = MaterialTheme.typography.titleMedium,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f),
                )
                if (memo.pinned) Badge("Pinned")
            }
            if (memo.preview.isNotEmpty()) {
                Text(
                    memo.preview,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                Text(
                    app.platform.relative(memo.updatedAt),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                if (memo.conflict) Badge("Conflict", warn = true)
                if (memo.pushError != null) Badge("Not synced", warn = true)
                if (memo.undecoded == "undecryptable") Badge("Cannot decrypt", warn = true)
                if (memo.shared) Badge("Shared")
                if (memo.images > 0) Badge(if (memo.images == 1) "1 image" else "${memo.images} images")
                if (memo.dirty && memo.pushError == null) Badge("Unsynced")
                Spacer(Modifier.weight(1f))
                for (tag in memo.tags.take(3)) {
                    Text("#$tag", style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.primary)
                }
            }
        }
    }
}

@Composable
fun Badge(text: String, warn: Boolean = false) {
    Text(
        text,
        style = MaterialTheme.typography.labelSmall,
        color = if (warn) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.secondary,
    )
}

@Composable
fun Spaced(width: Int = 8) = Spacer(Modifier.width(width.dp))

@Composable
fun SmallButton(text: String, enabled: Boolean = true, onClick: () -> Unit) {
    TextButton(onClick, enabled = enabled) { Text(text) }
}
