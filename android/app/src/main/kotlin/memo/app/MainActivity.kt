package memo.app

import android.content.ClipData
import android.content.ClipboardManager
import android.net.Uri
import android.os.Bundle
import android.text.format.DateUtils
import android.graphics.Color
import androidx.activity.ComponentActivity
import androidx.activity.SystemBarStyle
import androidx.activity.OnBackPressedCallback
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import java.text.DateFormat
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.util.Date
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import memo.ui.App
import memo.ui.AppState
import memo.ui.Platform

class MainActivity : ComponentActivity(), Platform {
    private val backend get() = (application as MemoApp).backend
    private var app: AppState? = null

    private var picked: CompletableDeferred<Uri?>? = null
    private val pickImage =
        registerForActivityResult(ActivityResultContracts.PickVisualMedia()) { picked?.complete(it) }

    private var created: CompletableDeferred<Uri?>? = null
    private var createMime = "application/octet-stream"
    private val createDocument =
        registerForActivityResult(
            object : ActivityResultContracts.CreateDocument("application/octet-stream") {
                override fun createIntent(context: android.content.Context, input: String) =
                    super.createIntent(context, input).setType(createMime)
            },
        ) { created?.complete(it) }

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        onBackPressedDispatcher.addCallback(
            this,
            object : OnBackPressedCallback(true) {
                override fun handleOnBackPressed() {
                    if (app?.back() != true) {
                        isEnabled = false
                        onBackPressedDispatcher.onBackPressed()
                        isEnabled = true
                    }
                }
            },
        )
        setContent { App(backend, this) { app = it } }
    }

    override fun onStart() {
        super.onStart()
        backend.setForeground(true)
    }

    override fun onStop() {
        backend.setForeground(false)
        super.onStop()
    }

    // ---- Platform ----

    override suspend fun pickImage(): String? {
        val result = CompletableDeferred<Uri?>()
        picked = result
        pickImage.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly))
        return result.await()?.toString()
    }

    override suspend fun saveFile(name: String, mime: String, bytes: ByteArray): Boolean {
        val result = CompletableDeferred<Uri?>()
        created = result
        createMime = mime
        createDocument.launch(name)
        val uri = result.await() ?: return false
        withContext(Dispatchers.IO) { contentResolver.openOutputStream(uri)?.use { it.write(bytes) } }
        return true
    }

    override fun darkBars(dark: Boolean) {
        val style =
            if (dark) SystemBarStyle.dark(Color.TRANSPARENT) else SystemBarStyle.light(Color.TRANSPARENT, Color.TRANSPARENT)
        enableEdgeToEdge(style, style)
    }

    override fun copy(text: String) {
        val clip = ClipData.newPlainText("memo", text)
        // Recovery keys: keep them out of the clipboard preview.
        if (android.os.Build.VERSION.SDK_INT >= 33) {
            clip.description.extras =
                android.os.PersistableBundle().apply { putBoolean(android.content.ClipDescription.EXTRA_IS_SENSITIVE, true) }
        }
        getSystemService(ClipboardManager::class.java).setPrimaryClip(clip)
    }

    override fun dateTime(ms: Long): String = DateFormat.getDateTimeInstance(DateFormat.MEDIUM, DateFormat.SHORT).format(Date(ms))

    override fun date(ms: Long): String = DateFormat.getDateInstance(DateFormat.MEDIUM).format(Date(ms))

    private fun local(ms: Long): LocalDate = Instant.ofEpochMilli(ms).atZone(ZoneId.systemDefault()).toLocalDate()

    private fun LocalDate.ms(): Long = atStartOfDay(ZoneId.systemDefault()).toInstant().toEpochMilli()

    override fun today() = LocalDate.now().ms()

    override fun dayStart(ms: Long) = local(ms).ms()

    override fun addDays(day: Long, days: Int) = local(day).plusDays(days.toLong()).ms()

    override fun monthStart(day: Long, months: Int) = local(day).withDayOfMonth(1).plusMonths(months.toLong()).ms()

    override fun weekday(day: Long) = local(day).dayOfWeek.value

    override fun dayOfMonth(day: Long) = local(day).dayOfMonth

    override fun monthName(day: Long): String = DateTimeFormatter.ofPattern("LLLL yyyy").format(local(day))

    override fun dayLabel(day: Long): String {
        val date = local(day)
        val today = LocalDate.now()
        return when (date) {
            today -> "Today"
            today.minusDays(1) -> "Yesterday"
            else ->
                DateTimeFormatter.ofPattern(if (date.year == today.year) "EEEE, d MMMM" else "EEEE, d MMMM yyyy").format(date)
        }
    }

    override fun relative(ms: Long): String {
        // Clocks differ a little between devices: nothing is in the future.
        val now = System.currentTimeMillis()
        if (now - ms < DateUtils.MINUTE_IN_MILLIS) return "just now"
        return DateUtils.getRelativeTimeSpanString(ms, now, DateUtils.MINUTE_IN_MILLIS).toString()
    }
}
