package memo.ui

// The read models from `memo_core::sync::view` and the API types the UI sees. Mirrors
// web/src/lib/types.ts.

data class MemoSummary(
    val id: String,
    val ownerId: String,
    val title: String,
    val preview: String,
    val tags: List<String>,
    val pinned: Boolean,
    val archived: Boolean,
    val shared: Boolean,
    val writable: Boolean,
    val createdAt: Long,
    val updatedAt: Long,
    val dirty: Boolean,
    val conflict: Boolean,
    /** `missing_key`, `undecryptable` or `newer_format`. */
    val undecoded: String?,
    /** `forbidden`, `invalid` or `quota_exceeded`. */
    val pushError: String?,
    val images: Int,
)

data class MemberView(
    val userId: String,
    val username: String,
    /** `read` or `write`. */
    val permission: String,
    /** `me`, `verified`, `pinned`, `not_pinned` or `changed`. */
    val trust: String,
    val fingerprint: String,
    /** On your own memo: the server lists them, but you never shared it with them. */
    val unconfirmed: Boolean = false,
)

data class ThumbRef(val mime: String, val width: Int, val height: Int)

data class AttachmentView(
    val id: String,
    val mime: String,
    val size: Long,
    val width: Int,
    val height: Int,
    val thumb: ThumbRef?,
    val available: Boolean,
    val thumbAvailable: Boolean,
)

data class ConflictWith(val body: String, val version: Long, val editedAt: Long)

data class MemoDetail(
    val summary: MemoSummary,
    val body: String,
    val editedAt: Long,
    val serverVersion: Long,
    val updatedBy: String?,
    val own: Boolean,
    val members: List<MemberView>,
    val attachments: List<AttachmentView>,
    val conflictWith: ConflictWith?,
    /** Memos that link to this one. */
    val linkedFrom: List<MemoLink>,
) {
    val id get() = summary.id
}

data class MemoLink(val id: String, val title: String)

/** `day` is a day picked in the calendar (local midnight, Unix ms). `has` holds quick filters. */
data class MemoFilter(
    val query: String = "",
    val tag: String? = null,
    val archived: Boolean = false,
    val has: List<String> = emptyList(),
    val day: Long? = null,
) {
    val narrowed get() = archived || tag != null || has.isNotEmpty() || day != null || query.isNotBlank()

    fun isSaved(saved: SavedFilter) =
        !archived && day == null && query.trim() == saved.query.trim() && tag == saved.tag && has.toSet() == saved.has.toSet()
}

/** The quick filters: what a memo must contain (`docs/crypto.md#vault-payload`). */
val QuickFilters = listOf("open_tasks" to "Open tasks", "tasks" to "Tasks", "links" to "Links", "images" to "Images", "code" to "Code")

data class SavedFilter(val id: String, val name: String, val query: String, val tag: String?, val has: List<String>) {
    fun toFilter() = MemoFilter(query = query, tag = tag, has = has)
}

/** Counts over the memo list (the archive only as `archived`). */
data class Stats(
    val memos: Int,
    val archived: Int,
    val words: Int,
    val tags: Int,
    val images: Int,
    val openTasks: Int,
    val doneTasks: Int,
    val shared: Int,
    val since: Long?,
)

data class TagCount(val tag: String, val count: Int)

data class Me(
    val userId: String,
    val username: String,
    val role: String,
    val quotaBytes: Long,
    val usedBytes: Long,
    val createdAt: Long,
)

data class Device(
    val id: String,
    val name: String,
    val kind: String,
    val createdAt: Long,
    val lastSeenAt: Long,
    val current: Boolean,
)

/** A user from `GET /users/lookup`, with how far this device trusts their key. */
data class Contact(
    val userId: String,
    val username: String,
    val publicKey: String,
    val trust: String,
    val fingerprint: String,
)

data class RevisionInfo(
    val version: Long,
    val keyVersion: Int,
    val updatedBy: String?,
    val updatedAt: Long,
    val size: Long,
)

/** `memo_core::sync::SyncEvent`; the fields a type does not have are null. */
data class SyncEvent(
    val type: String,
    val memoId: String? = null,
    val copy: String? = null,
    val reason: String? = null,
    val userId: String? = null,
)

sealed interface Session {
    data object Starting : Session

    data object SignedOut : Session

    /** Registered or recovered: the recovery key must be confirmed before the account is used. */
    data class Onboarding(val recoveryKey: String) : Session

    data class Unlocked(val server: String, val username: String, val userId: String, val fingerprint: String) :
        Session
}

data class SyncStatus(
    val syncing: Boolean = false,
    /** Unix ms of the last successful sync. */
    val last: Long? = null,
    val error: String? = null,
    val unsynced: Boolean = false,
)

// ---- Rendered bodies: `memo_core::render::Block` ----

sealed interface Block {
    data class Paragraph(val content: List<Inline>) : Block

    data class Heading(val level: Int, val content: List<Inline>) : Block

    data class Quote(val blocks: List<Block>) : Block

    data class ListBlock(val start: Long?, val items: List<ListItem>) : Block

    /** `spans` split `text` by highlight token (null: plain) when the language is known. */
    data class Code(val text: String, val spans: List<CodeSpan> = emptyList()) : Block

    data class Table(val head: List<List<Inline>>, val rows: List<List<List<Inline>>>) : Block

    data object Rule : Block
}

data class ListItem(val task: Task?, val blocks: List<Block>)

/** `token`: `comment`, `string`, `number`, `constant`, `keyword`, `function` or `type`. */
data class CodeSpan(val text: String, val token: String?)

/** `offset` goes to [Backend.toggleTask]. */
data class Task(val checked: Boolean, val offset: Int)

sealed interface Inline {
    data class Text(
        val text: String,
        val bold: Boolean = false,
        val italic: Boolean = false,
        val strike: Boolean = false,
        val code: Boolean = false,
        val link: String? = null,
    ) : Inline

    data class Tag(val name: String, val tag: String) : Inline

    data object Break : Inline

    data class Image(val attachment: String, val alt: String) : Inline
}
