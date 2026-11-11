package memo.app

import memo.ui.AttachmentView
import memo.ui.Block
import memo.ui.CodeSpan
import memo.ui.ConflictWith
import memo.ui.Device
import memo.ui.Inline
import memo.ui.ListItem
import memo.ui.Me
import memo.ui.MemberView
import memo.ui.MemoDetail
import memo.ui.MemoLink
import memo.ui.MemoSummary
import memo.ui.RevisionInfo
import memo.ui.SavedFilter
import memo.ui.Stats
import memo.ui.SyncEvent
import memo.ui.TagCount
import memo.ui.Task
import memo.ui.ThumbRef
import org.json.JSONArray
import org.json.JSONObject

// JSON from memo-core and the API into the UI's models.

fun JSONObject.str(key: String): String? = if (isNull(key)) null else getString(key)

fun <T> JSONArray.map(f: (JSONObject) -> T): List<T> = (0 until length()).map { f(getJSONObject(it)) }

fun JSONArray.strings(): List<String> = (0 until length()).map { getString(it) }

fun JSONObject.toSummary() =
    MemoSummary(
        id = getString("id"),
        ownerId = getString("owner_id"),
        title = getString("title"),
        preview = getString("preview"),
        tags = getJSONArray("tags").strings(),
        pinned = getBoolean("pinned"),
        archived = getBoolean("archived"),
        shared = getBoolean("shared"),
        writable = getBoolean("writable"),
        createdAt = getLong("created_at"),
        updatedAt = getLong("updated_at"),
        dirty = getBoolean("dirty"),
        conflict = getBoolean("conflict"),
        undecoded = str("undecoded"),
        pushError = str("push_error"),
        images = getInt("images"),
    )

fun JSONObject.toDetail() =
    MemoDetail(
        summary = toSummary(),
        body = getString("body"),
        editedAt = getLong("edited_at"),
        serverVersion = getLong("server_version"),
        updatedBy = str("updated_by"),
        own = getBoolean("own"),
        members =
            getJSONArray("members").map {
                MemberView(
                    it.getString("user_id"),
                    it.getString("username"),
                    it.getString("permission"),
                    it.getString("trust"),
                    it.getString("fingerprint"),
                )
            },
        attachments =
            getJSONArray("attachments").map {
                AttachmentView(
                    id = it.getString("id"),
                    mime = it.getString("mime"),
                    size = it.getLong("size"),
                    width = it.getInt("width"),
                    height = it.getInt("height"),
                    thumb =
                        it.optJSONObject("thumb")?.let { t ->
                            ThumbRef(t.getString("mime"), t.getInt("width"), t.getInt("height"))
                        },
                    available = it.getBoolean("available"),
                    thumbAvailable = it.getBoolean("thumb_available"),
                )
            },
        conflictWith =
            optJSONObject("conflict_with")?.let {
                ConflictWith(it.getString("body"), it.getLong("version"), it.getLong("edited_at"))
            },
        linkedFrom = getJSONArray("linked_from").map { MemoLink(it.getString("id"), it.getString("title")) },
    )

fun JSONObject.toSavedFilter() =
    SavedFilter(
        id = getString("id"),
        name = getString("name"),
        query = getString("query"),
        tag = str("tag"),
        has = getJSONArray("has").strings(),
    )

fun JSONObject.toStats() =
    Stats(
        memos = getInt("memos"),
        archived = getInt("archived"),
        words = getInt("words"),
        tags = getInt("tags"),
        images = getInt("images"),
        openTasks = getInt("open_tasks"),
        doneTasks = getInt("done_tasks"),
        shared = getInt("shared"),
        since = if (isNull("since")) null else getLong("since"),
    )

fun JSONObject.toTagCount() = TagCount(getString("tag"), getInt("count"))

fun JSONObject.toMe() =
    Me(
        userId = getString("user_id"),
        username = getString("username"),
        role = getString("role"),
        quotaBytes = getLong("quota_bytes"),
        usedBytes = getLong("used_bytes"),
        createdAt = getLong("created_at"),
    )

fun JSONObject.toDevice() =
    Device(
        id = getString("id"),
        name = getString("name"),
        kind = getString("kind"),
        createdAt = getLong("created_at"),
        lastSeenAt = getLong("last_seen_at"),
        current = getBoolean("current"),
    )

fun JSONObject.toRevision() =
    RevisionInfo(
        version = getLong("version"),
        keyVersion = getInt("key_version"),
        updatedBy = str("updated_by"),
        updatedAt = getLong("updated_at"),
        size = getLong("size"),
    )

fun JSONObject.toEvent() =
    SyncEvent(
        type = getString("type"),
        memoId = str("memo_id") ?: str("original"),
        copy = str("copy"),
        reason = str("reason"),
        userId = str("user_id"),
    )

fun JSONObject.toBlock(): Block =
    when (getString("type")) {
        "paragraph" -> Block.Paragraph(getJSONArray("content").map { it.toInline() })
        "heading" -> Block.Heading(getInt("level"), getJSONArray("content").map { it.toInline() })
        "quote" -> Block.Quote(getJSONArray("blocks").map { it.toBlock() })
        "list" ->
            Block.ListBlock(
                if (isNull("start")) null else getLong("start"),
                getJSONArray("items").map { item ->
                    ListItem(
                        item.optJSONObject("task")?.let { Task(it.getBoolean("checked"), it.getInt("offset")) },
                        item.getJSONArray("blocks").map { it.toBlock() },
                    )
                },
            )
        "code" ->
            Block.Code(
                getString("text"),
                optJSONArray("spans")?.map { CodeSpan(it.getString("text"), it.str("token")) } ?: emptyList(),
            )
        "table" -> {
            fun cells(row: JSONArray) = (0 until row.length()).map { row.getJSONArray(it).map { c -> c.toInline() } }
            val rows = getJSONArray("rows")
            Block.Table(cells(getJSONArray("head")), (0 until rows.length()).map { cells(rows.getJSONArray(it)) })
        }
        else -> Block.Rule
    }

fun JSONObject.toInline(): Inline =
    when (getString("type")) {
        "tag" -> Inline.Tag(getString("name"), getString("tag"))
        "break" -> Inline.Break
        "image" -> Inline.Image(getString("attachment"), getString("alt"))
        else ->
            Inline.Text(
                text = getString("text"),
                bold = optBoolean("bold"),
                italic = optBoolean("italic"),
                strike = optBoolean("strike"),
                code = optBoolean("code"),
                link = str("link"),
            )
    }
