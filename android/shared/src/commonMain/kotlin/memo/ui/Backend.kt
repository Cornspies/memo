package memo.ui

import androidx.compose.ui.graphics.ImageBitmap
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow

/**
 * What the UI talks to. The Android app implements it with memo-core over JNI, a local SQLite
 * store and its own HTTP; see web/src/lib/types.ts for the web's version of the same interface.
 */
interface Backend {
    val session: StateFlow<Session>
    val sync: StateFlow<SyncStatus>
    /** Bumped whenever local data changed (an edit or a sync): views reload. */
    val version: StateFlow<Long>
    val events: SharedFlow<List<SyncEvent>>

    suspend fun login(server: String, username: String, password: String)
    /** Returns the recovery key to show; the session is [Session.Onboarding] until [finishOnboarding]. */
    suspend fun register(server: String, username: String, password: String, invite: String): String
    /** Returns the new recovery key. */
    suspend fun recover(server: String, username: String, recoveryKey: String, newPassword: String): String
    suspend fun finishOnboarding()
    /** Signs out and deletes this device's copy, unsynced changes included. */
    suspend fun logout()
    fun passwordProblem(password: String): String?

    suspend fun sync()
    /** The app came to the foreground. */
    fun poke()

    suspend fun list(filter: MemoFilter): List<MemoSummary>
    suspend fun tags(): List<TagCount>
    /** Creation times of the memos in the main list, oldest first. */
    suspend fun activity(): List<Long>
    suspend fun stats(): Stats
    suspend fun savedFilters(): List<SavedFilter>
    /** Saves the filter's query, tag and quick filters under `name`. Returns the new id. */
    suspend fun saveFilter(name: String, filter: MemoFilter): String
    suspend fun deleteFilter(id: String)
    suspend fun memo(id: String): MemoDetail
    suspend fun render(body: String): List<Block>
    /** `kind`: `original` or `thumbnail`. Null if not available yet (not uploaded, or offline). */
    suspend fun image(memoId: String, attachmentId: String, kind: String): ImageBitmap?

    suspend fun create(body: String): String
    suspend fun edit(id: String, body: String, prune: Boolean)
    suspend fun toggleTask(id: String, offset: Int): Boolean
    suspend fun remove(id: String)
    suspend fun setState(id: String, pinned: Boolean, archived: Boolean)
    suspend fun resolve(id: String, body: String)
    suspend fun saveAsCopy(id: String): String
    /** `image` comes from [Platform.pickImage]. Returns the attachment id. */
    suspend fun attachImage(memoId: String, image: String): String

    suspend fun revisions(id: String): List<RevisionInfo>
    suspend fun revision(id: String, version: Long): String
    suspend fun restoreRevision(id: String, version: Long)

    suspend fun lookupUser(username: String): Contact
    suspend fun share(memoId: String, user: Contact, permission: String)
    suspend fun setPermission(memoId: String, userId: String, permission: String)
    suspend fun revoke(memoId: String, userIds: List<String>)
    suspend fun leave(memoId: String)
    suspend fun verifyContact(userId: String)

    suspend fun me(): Me
    suspend fun devices(): List<Device>
    suspend fun removeDevice(id: String)
    suspend fun changePassword(current: String, next: String, revokeOthers: Boolean)
    suspend fun deleteAccount(password: String)
    /** A ZIP of every memo as Markdown, with its images. */
    suspend fun exportAll(): ByteArray

    /** Downscale images to 2560 px on the longest side before attaching (default on). */
    var downscale: Boolean
    /** `system`, `light` or `dark`, on this device. */
    var theme: String
}

/** Errors from the backend: `code` is an API error code or a memo-core error name. */
class BackendException(val code: String, message: String, val status: Int = 0) : Exception(message)

/** What only the platform can do. */
interface Platform {
    /** Opens the image picker. Returns a reference for [Backend.attachImage], or null if cancelled. */
    suspend fun pickImage(): String?
    /** Asks where to save the file. False if cancelled. */
    suspend fun saveFile(name: String, mime: String, bytes: ByteArray): Boolean
    fun copy(text: String)
    /** Light or dark status and navigation bar icons, to match the app's theme. */
    fun darkBars(dark: Boolean)
    fun dateTime(ms: Long): String
    /** "11 Nov 2026". */
    fun date(ms: Long): String
    /** "5 minutes ago". */
    fun relative(ms: Long): String

    // Local calendar days, as the local midnight in Unix ms.

    fun today(): Long
    fun dayStart(ms: Long): Long
    /** The day `days` after `day`. */
    fun addDays(day: Long, days: Int): Long
    /** The first day of the month `months` after the one `day` falls in. */
    fun monthStart(day: Long, months: Int = 0): Long
    /** 1 for Monday to 7 for Sunday. */
    fun weekday(day: Long): Int
    fun dayOfMonth(day: Long): Int
    /** "November 2026". */
    fun monthName(day: Long): String
    /** "Today", "Yesterday", or "Monday, 3 November" (with the year if not this year). */
    fun dayLabel(day: Long): String
}
