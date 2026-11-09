package memo.app

import android.content.Context
import android.graphics.BitmapFactory
import android.net.ConnectivityManager
import android.net.Network
import android.net.Uri
import android.os.Build
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import java.io.ByteArrayOutputStream
import java.io.File
import java.net.URLEncoder
import java.util.zip.ZipEntry
import java.util.zip.ZipOutputStream
import kotlin.math.min
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import memo.ui.Backend
import memo.ui.BackendException
import memo.ui.Block
import memo.ui.Contact
import memo.ui.Device
import memo.ui.Me
import memo.ui.MemoDetail
import memo.ui.MemoFilter
import memo.ui.MemoSummary
import memo.ui.RevisionInfo
import memo.ui.Session
import memo.ui.SyncEvent
import memo.ui.SyncStatus
import memo.ui.TagCount
import org.json.JSONArray
import org.json.JSONObject

private const val EDIT_DELAY = 2_000L
private const val PERIODIC = 5 * 60_000L
private const val MAX_BACKOFF = 5 * 60_000L
/** Downloaded image blobs kept on the device (`docs/sync.md#attachments-transfer`). */
private const val BLOB_CACHE = 1L shl 30

/**
 * The Android backend: memo-core (crates/android) with the account's SQLite store, HTTP to the
 * server, and sync scheduling (`docs/sync.md#when-sync-runs`). Unlike the web client it works
 * offline: everything is local, and sync catches up when the server is reachable.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class AndroidBackend(private val context: Context) : Backend {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    /** memo-core calls, one at a time. */
    private val core = Dispatchers.IO.limitedParallelism(1)
    private val accounts = AccountFile(context)
    private val prefs = context.getSharedPreferences("memo", Context.MODE_PRIVATE)

    private var account: Account? = null
    private var http: Http? = null
    /** Registered or recovered: waits for the recovery key to be confirmed. */
    private var onboarding: Account? = null
    private var foreground = false

    override val session = MutableStateFlow<Session>(Session.Starting)
    override val sync = MutableStateFlow(SyncStatus())
    override val version = MutableStateFlow(0L)
    override val events = MutableSharedFlow<List<SyncEvent>>(extraBufferCapacity = 16)

    override var downscale: Boolean
        get() = prefs.getBoolean("downscale", true)
        set(value) = prefs.edit().putBoolean("downscale", value).apply()

    // Declared before `init`, which starts the sync loop.
    private val requests = Channel<CompletableDeferred<Unit>>(Channel.UNLIMITED)
    private var scheduled: Job? = null
    private var failures = 0
    private val downloads = mutableMapOf<String, CompletableDeferred<Unit>>()

    init {
        scope.launch {
            withContext(core) {
                Core.call("init", "dir" to File(context.noBackupFilesDir, "db").path)
                accounts.read()?.let { saved ->
                    try {
                        open(saved, Core.call("open", "secrets" to saved.secrets).obj)
                    } catch (e: BackendException) {
                        null
                    }
                }
            } ?: run { session.value = Session.SignedOut }
            if (account != null) syncQuietly()
        }
        scope.launch { syncLoop() }
        watchNetwork()
    }

    private suspend fun <T> core(block: () -> T): T = withContext(core) { block() }

    private suspend fun <T> io(block: () -> T): T = withContext(Dispatchers.IO) { block() }

    private fun api(): Http = http ?: throw BackendException("locked", "Sign in first.")

    private fun now() = System.currentTimeMillis()

    private fun device(): JSONObject {
        val maker = Build.MANUFACTURER.replaceFirstChar { it.uppercase() }
        val model = if (Build.MODEL.startsWith(maker, ignoreCase = true)) Build.MODEL else "$maker ${Build.MODEL}"
        return JSONObject().put("name", model).put("kind", "android")
    }

    // ---- Session ----

    /** Opened by memo-core; `opened` holds the secrets to keep. Returns the account. */
    private fun open(saved: Account, opened: JSONObject): Account {
        val next = saved.copy(secrets = opened.getJSONObject("secrets"))
        account = next
        http = Http(next.server, next.token)
        session.value = Session.Unlocked(next.server, next.username, next.userId, opened.getString("fingerprint"))
        return next
    }

    private suspend fun signIn(server: Http, username: String, token: String, opened: JSONObject, confirm: String?) {
        val next =
            Account(server.server, username.trim().lowercase(), opened.getString("user_id"), token, opened.getJSONObject("secrets"))
        core { accounts.write(next) }
        if (confirm != null) {
            account = next
            http = Http(next.server, next.token)
            onboarding = next
            session.value = Session.Onboarding(confirm)
        } else {
            core { open(next, opened) }
            version.update { it + 1 }
            scope.launch { syncQuietly() }
        }
    }

    private suspend fun prelogin(server: Http, username: String): JSONObject =
        io { server.json("POST", "/auth/prelogin", JSONObject().put("username", username.trim())).json }

    override suspend fun login(server: String, username: String, password: String) {
        val api = Http(server)
        val prelogin = prelogin(api, username)
        val request = core {
            Core.call("login_start", "password" to password, "prelogin" to prelogin, "username" to username, "device" to device()).obj
        }
        val response = io { api.json("POST", "/auth/login", request).json }
        val opened = core { Core.call("login_finish", "response" to response).obj }
        signIn(api, username, response.getString("token"), opened, confirm = null)
    }

    override suspend fun register(server: String, username: String, password: String, invite: String): String {
        val api = Http(server)
        val started = core {
            Core.call(
                "register_start",
                "password" to password,
                "username" to username,
                "invite" to invite,
                "device" to device(),
            ).obj
        }
        val response = io { api.json("POST", "/auth/register", started.getJSONObject("request")).json }
        val opened = core { Core.call("register_finish").obj }
        val key = started.getString("recovery_key")
        signIn(api, username, response.getString("token"), opened, confirm = key)
        return key
    }

    override suspend fun recover(server: String, username: String, recoveryKey: String, newPassword: String): String {
        val api = Http(server)
        val request = core { Core.call("recover_start", "recovery_key" to recoveryKey, "username" to username).obj }
        val response = io { api.json("POST", "/auth/recover", request).json }
        val completed = core {
            Core.call("recover_complete", "response" to response, "new_password" to newPassword, "device" to device()).obj
        }
        val done = io { api.json("POST", "/auth/recover/complete", completed.getJSONObject("request")).json }
        val opened = core { Core.call("recover_finish").obj }
        val key = completed.getString("recovery_key")
        signIn(api, username, done.getString("token"), opened, confirm = key)
        return key
    }

    override suspend fun finishOnboarding() {
        val saved = onboarding ?: throw BackendException("not_found", "Nothing to finish.")
        onboarding = null
        core { open(saved, Core.call("open", "secrets" to saved.secrets).obj) }
        version.update { it + 1 }
        scope.launch { syncQuietly() }
    }

    override suspend fun logout() {
        val api = http
        if (api != null) runCatching { io { api.call("POST", "/auth/logout") } }
        wipe()
    }

    /** Forgets the account and deletes this device's copy (logout, or the device was signed out). */
    private suspend fun wipe() {
        val userId = account?.userId
        core {
            if (userId != null) Core.call("forget", "user_id" to userId) else Core.call("close")
            accounts.delete()
        }
        account = null
        http = null
        onboarding = null
        scheduled?.cancel()
        SyncWorker.cancel(context)
        sync.value = SyncStatus()
        session.value = Session.SignedOut
    }

    override fun passwordProblem(password: String): String? =
        Core.call("password_problem", "password" to password).let { if (it.isNull) null else it.string }

    // ---- Sync ----

    /** Runs a sync now, or once more after the one in progress, and waits for it. Throws its error. */
    private suspend fun requestSync() {
        val done = CompletableDeferred<Unit>()
        requests.send(done)
        done.await()
    }

    /** For syncs nobody waits on: the status line shows the error. */
    private suspend fun syncQuietly() {
        try {
            requestSync()
        } catch (e: BackendException) {
            // Shown in `sync`.
        }
    }

    /** Sessions run one after another; requests that arrive during one share the next. */
    private suspend fun syncLoop() {
        while (true) {
            val waiting = mutableListOf(requests.receive())
            while (true) waiting.add(requests.tryReceive().getOrNull() ?: break)
            val error =
                try {
                    syncRound()
                    null
                } catch (e: BackendException) {
                    e
                } catch (e: Exception) {
                    android.util.Log.e("memo", "sync failed", e)
                    BackendException("internal", e.toString())
                }
            waiting.forEach { if (error == null) it.complete(Unit) else it.completeExceptionally(error) }
        }
    }

    private fun schedule(delayMs: Long) {
        if (account == null) return
        scheduled?.cancel()
        scheduled = scope.launch {
            delay(delayMs)
            syncQuietly()
        }
    }

    private suspend fun syncRound() {
        if (account == null || session.value !is Session.Unlocked) return
        scheduled?.cancel()
        sync.update { it.copy(syncing = true) }
        try {
            val found = syncOnce()
            failures = 0
            sync.update { it.copy(last = now(), error = null) }
            if (found.isNotEmpty()) events.emit(found)
            version.update { it + 1 }
            if (foreground) schedule(PERIODIC)
        } catch (e: BackendException) {
            if (e.code == "unauthenticated") {
                // This device was signed out (or the account deleted) elsewhere: its copy goes too.
                wipe()
                events.emit(listOf(SyncEvent("signed_out")))
                return
            }
            failures++
            sync.update { it.copy(error = memo.ui.errorText(e)) }
            if (foreground) schedule(min(MAX_BACKOFF, 5_000L shl (failures - 1).coerceAtMost(10)))
            throw e
        } finally {
            val unsynced = account != null && runCatching { core { Core.call("has_unsynced").bool } }.getOrDefault(false)
            sync.update { it.copy(syncing = false, unsynced = unsynced) }
        }
    }

    private suspend fun syncOnce(): List<SyncEvent> {
        val api = api()
        var body: Any? = core { Core.call("sync_start", "now" to now()).obj }
        val found = mutableListOf<SyncEvent>()
        try {
            while (body is JSONObject) {
                val request = body
                val response = io { api.json("POST", "/sync", request).json }
                body = core { Core.call("sync_handle", "response" to response).raw }
            }
        } finally {
            core { Core.call("sync_finish").array }.let { list -> found += list.map { it.toEvent() } }
        }
        val uploads = core { Core.call("pending_uploads").array }
        for (upload in uploads.map { it }) {
            val id = upload.getString("attachment_id")
            val kind = upload.getString("kind")
            val blob = core { Core.call("blob", "attachment_id" to id, "kind" to kind).data } ?: continue
            try {
                val memoId = upload.getString("memo_id")
                io { api.call("PUT", "/memos/$memoId/attachments/$id/$kind", blob, "application/octet-stream") }
                core { Core.call("upload_done", "attachment_id" to id, "kind" to kind) }
            } catch (e: BackendException) {
                // Too large or over quota: retrying every sync will not help; the image stays local.
                if (e.code == "too_large" || e.code == "quota_exceeded") {
                    found += SyncEvent("push_rejected", memoId = upload.getString("memo_id"), reason = "quota_exceeded")
                    continue
                }
                throw e
            }
        }
        core { Core.call("trim_blobs", "budget" to BLOB_CACHE) }
        return found.filter { it.type != "rollback" }
    }

    override suspend fun sync() = requestSync()

    override fun poke() {
        if (account != null) schedule(0)
    }

    /** The app came to the foreground or went to the background. */
    fun setForeground(value: Boolean) {
        foreground = value
        if (value) {
            poke()
        } else {
            scheduled?.cancel()
            if (account != null) SyncWorker.schedule(context, soon = sync.value.unsynced)
        }
    }

    /** For the background worker. False if it should be retried later. */
    suspend fun syncInBackground(): Boolean {
        if (account == null) return true
        return try {
            requestSync()
            true
        } catch (e: BackendException) {
            false
        }
    }

    private fun watchNetwork() {
        val manager = context.getSystemService(ConnectivityManager::class.java) ?: return
        manager.registerDefaultNetworkCallback(
            object : ConnectivityManager.NetworkCallback() {
                override fun onAvailable(network: Network) {
                    if (foreground && failures > 0) poke()
                }
            },
        )
    }

    /** A local change: sync soon, and tell the views. */
    private suspend fun edited() {
        version.update { it + 1 }
        val unsynced = core { Core.call("has_unsynced").bool }
        sync.update { it.copy(unsynced = unsynced) }
        schedule(EDIT_DELAY)
    }

    // ---- Memos ----

    override suspend fun list(filter: MemoFilter): List<MemoSummary> = core {
        val json = JSONObject().put("query", filter.query).put("tag", filter.tag ?: JSONObject.NULL).put("archived", filter.archived)
        Core.call("memo_list", "filter" to json).array.map { it.toSummary() }
    }

    override suspend fun tags(): List<TagCount> = core { Core.call("tag_counts").array.map { it.toTagCount() } }

    override suspend fun memo(id: String): MemoDetail = core { Core.call("memo_detail", "memo_id" to id).obj.toDetail() }

    override suspend fun render(body: String): List<Block> = core { Core.call("render", "body" to body).array.map { it.toBlock() } }

    /** Decrypted bytes, downloading the blob first if needed. Null if not on the server yet. */
    private suspend fun attachmentBytes(memoId: String, attachmentId: String, kind: String): ByteArray? {
        val local = core { Core.call("attachment", "memo_id" to memoId, "attachment_id" to attachmentId, "kind" to kind).data }
        if (local != null) return local
        val key = "$attachmentId/$kind"
        val (download, mine) =
            synchronized(downloads) {
                downloads[key]?.let { it to false } ?: CompletableDeferred<Unit>().also { downloads[key] = it }.let { it to true }
            }
        if (mine) {
            try {
                val envelope = io { api().call("GET", "/attachments/$attachmentId/$kind").body }
                core { Core.call("put_blob", "attachment_id" to attachmentId, "kind" to kind, data = envelope) }
                download.complete(Unit)
            } catch (e: Throwable) {
                download.completeExceptionally(e)
            } finally {
                synchronized(downloads) { downloads.remove(key) }
            }
        }
        try {
            download.await()
        } catch (e: BackendException) {
            // Not uploaded yet.
            if (e.code == "not_found") return null
            throw e
        }
        return core { Core.call("attachment", "memo_id" to memoId, "attachment_id" to attachmentId, "kind" to kind).data }
            ?: throw BackendException("decrypt", "The image cannot be decrypted.")
    }

    override suspend fun image(memoId: String, attachmentId: String, kind: String): ImageBitmap? {
        val bytes = attachmentBytes(memoId, attachmentId, kind) ?: return null
        val view = memo(memoId).attachments.find { it.id == attachmentId }
        val mime = if (kind == "thumbnail") view?.thumb?.mime else view?.mime
        val renderable = mime != null && core { Core.call("renderable", "mime" to mime, data = bytes).bool }
        if (!renderable) throw BackendException("bad_image", "This attachment is not an image that can be shown.")
        return io { BitmapFactory.decodeByteArray(bytes, 0, bytes.size)?.asImageBitmap() }
            ?: throw BackendException("bad_image", "The image cannot be decoded.")
    }

    override suspend fun create(body: String): String =
        core { Core.call("create", "body" to body, "now" to now()).string }.also { edited() }

    override suspend fun edit(id: String, body: String, prune: Boolean) {
        core { Core.call("edit", "memo_id" to id, "body" to body, "now" to now(), "prune" to prune) }
        edited()
    }

    override suspend fun toggleTask(id: String, offset: Int): Boolean {
        val done = core { Core.call("toggle_task", "memo_id" to id, "offset" to offset, "now" to now()).bool }
        if (done) edited()
        return done
    }

    override suspend fun remove(id: String) {
        core { Core.call("delete", "memo_id" to id) }
        edited()
    }

    override suspend fun setState(id: String, pinned: Boolean, archived: Boolean) {
        core { Core.call("set_state", "memo_id" to id, "pinned" to pinned, "archived" to archived) }
        edited()
    }

    override suspend fun resolve(id: String, body: String) {
        core { Core.call("resolve", "memo_id" to id, "body" to body, "now" to now()) }
        edited()
    }

    override suspend fun saveAsCopy(id: String): String =
        core { Core.call("save_as_copy", "memo_id" to id, "now" to now()).string }.also { edited() }

    override suspend fun attachImage(memoId: String, image: String): String {
        val input =
            io {
                context.contentResolver.openInputStream(Uri.parse(image))?.use { stream ->
                    val out = ByteArrayOutputStream()
                    val buffer = ByteArray(64 * 1024)
                    while (true) {
                        val n = stream.read(buffer)
                        if (n < 0) break
                        out.write(buffer, 0, n)
                        if (out.size() > MAX_IMAGE) throw BackendException("too_large", "Images can be at most 25 MB.")
                    }
                    out.toByteArray()
                }
            } ?: throw BackendException("not_found", "The image cannot be read.")
        // Decoding takes a while; Core.call is safe from any thread (the library locks).
        val prepared = withContext(Dispatchers.Default) { prepareImage(input, if (downscale) DOWNSCALE_SIDE else null) }
        val image0 = prepared.image
        val thumb = prepared.thumb
        val data = if (thumb != null) image0.bytes + thumb.bytes else image0.bytes
        val thumbMeta = thumb?.let { JSONObject().put("mime", it.mime).put("width", it.width).put("height", it.height) }
        val id = core {
            Core.call(
                "add_attachment",
                "memo_id" to memoId,
                "mime" to image0.mime,
                "width" to image0.width,
                "height" to image0.height,
                "size" to image0.bytes.size,
                "thumb" to thumbMeta,
                data = data,
            ).string
        }
        edited()
        return id
    }

    // ---- Revisions ----

    override suspend fun revisions(id: String): List<RevisionInfo> =
        io { JSONArray(api().call("GET", "/memos/$id/revisions").text).map { it.toRevision() } }

    private suspend fun fetchRevision(id: String, version: Long): JSONObject =
        io { api().call("GET", "/memos/$id/revisions/$version").json }

    override suspend fun revision(id: String, version: Long): String {
        val revision = fetchRevision(id, version)
        return core { Core.call("open_revision", "memo_id" to id, "revision" to revision).string }
    }

    override suspend fun restoreRevision(id: String, version: Long) {
        val revision = fetchRevision(id, version)
        core { Core.call("restore_revision", "memo_id" to id, "revision" to revision, "now" to now()) }
        edited()
    }

    // ---- Sharing ----

    private fun Contact.toUserInfo() = JSONObject().put("user_id", userId).put("username", username).put("public_key", publicKey)

    override suspend fun lookupUser(username: String): Contact {
        val name = URLEncoder.encode(username.trim(), "UTF-8")
        val user = io { api().call("GET", "/users/lookup?username=$name").json }
        val trust = core { Core.call("contact_trust", "user" to user).obj }
        return Contact(
            user.getString("user_id"),
            user.getString("username"),
            user.getString("public_key"),
            trust.getString("trust"),
            trust.getString("fingerprint"),
        )
    }

    override suspend fun share(memoId: String, user: Contact, permission: String) {
        // The memo must be on the server.
        requestSync()
        val request = core {
            Core.call(
                "prepare_share",
                "memo_id" to memoId,
                "user" to user.toUserInfo(),
                "permission" to permission,
                "now" to now(),
            ).obj
        }
        io { api().json("POST", "/memos/$memoId/members", request) }
        syncQuietly()
    }

    override suspend fun setPermission(memoId: String, userId: String, permission: String) {
        io { api().json("PATCH", "/memos/$memoId/members/$userId", JSONObject().put("permission", permission)) }
        syncQuietly()
    }

    override suspend fun revoke(memoId: String, userIds: List<String>) {
        var attempt = 0
        while (true) {
            requestSync()
            val request = core { Core.call("prepare_revoke", "memo_id" to memoId, "user_ids" to JSONArray(userIds)).obj }
            try {
                val response = io { api().json("POST", "/memos/$memoId/revoke", request).json }
                core { Core.call("apply_revoke", "memo_id" to memoId, "response" to response) }
                break
            } catch (e: BackendException) {
                // Someone wrote in the meantime: sync and re-encrypt.
                if (e.code != "conflict" || attempt++ >= 2) throw e
            }
        }
        syncQuietly()
    }

    override suspend fun leave(memoId: String) {
        io { api().call("DELETE", "/memos/$memoId/members/me") }
        syncQuietly()
    }

    override suspend fun verifyContact(userId: String) {
        core { Core.call("verify_contact", "user_id" to userId) }
        edited()
    }

    // ---- Account ----

    override suspend fun me(): Me = io { api().call("GET", "/me").json.toMe() }

    override suspend fun devices(): List<Device> = io { JSONArray(api().call("GET", "/devices").text).map { it.toDevice() } }

    override suspend fun removeDevice(id: String) {
        io { api().call("DELETE", "/devices/$id") }
    }

    private suspend fun ownPrelogin(): JSONObject {
        val username = account?.username ?: throw BackendException("locked", "Sign in first.")
        return prelogin(api(), username)
    }

    override suspend fun changePassword(current: String, next: String, revokeOthers: Boolean) {
        val prelogin = ownPrelogin()
        val request = core {
            Core.call(
                "password_change",
                "current_password" to current,
                "prelogin" to prelogin,
                "new_password" to next,
                "revoke_other_devices" to revokeOthers,
            ).obj
        }
        io { api().json("POST", "/auth/password", request) }
    }

    override suspend fun deleteAccount(password: String) {
        val prelogin = ownPrelogin()
        val request = core { Core.call("delete_account", "password" to password, "prelogin" to prelogin).obj }
        io { api().json("DELETE", "/me", request) }
        wipe()
    }

    // ---- Export ----

    private fun extension(mime: String) =
        mapOf("image/jpeg" to "jpg", "image/png" to "png", "image/webp" to "webp", "image/gif" to "gif")[mime] ?: "bin"

    private fun fileName(title: String, id: String): String {
        val slug = title.replace(Regex("[\\\\/:*?\"<>|#\\u0000-\\u001f]"), "").trim().take(60)
        return "${slug.ifEmpty { "untitled" }} (${id.takeLast(8)})"
    }

    override suspend fun exportAll(): ByteArray {
        val all = list(MemoFilter()) + list(MemoFilter(archived = true))
        val out = ByteArrayOutputStream()
        ZipOutputStream(out).use { zip ->
            for (summary in all) {
                val detail = memo(summary.id)
                var body = detail.body
                for (a in detail.attachments) {
                    val name = "images/${a.id}.${extension(a.mime)}"
                    val bytes = runCatching { attachmentBytes(detail.id, a.id, "original") }.getOrNull() ?: continue
                    zip.putNextEntry(ZipEntry(name))
                    zip.write(bytes)
                    zip.closeEntry()
                    body = body.replace("attachment:${a.id}", name)
                }
                val folder = if (summary.archived) "archive/" else ""
                zip.putNextEntry(ZipEntry("$folder${fileName(summary.title, summary.id)}.md"))
                zip.write(body.toByteArray())
                zip.closeEntry()
            }
        }
        return out.toByteArray()
    }
}
