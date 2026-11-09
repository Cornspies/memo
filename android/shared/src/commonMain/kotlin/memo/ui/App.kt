package memo.ui

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.SnackbarDuration
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.SnackbarResult
import androidx.compose.material3.Surface
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch

sealed interface Screen {
    data object List : Screen

    data class Memo(val id: String, val startEditing: Boolean = false) : Screen

    data object Settings : Screen
}

/** Navigation, notifications, and a scope that outlives single screens (saving on close). */
class AppState(val backend: Backend, val platform: Platform, val scope: CoroutineScope) {
    val stack = mutableStateListOf<Screen>(Screen.List)
    val screen get() = stack.last()
    val snackbar = SnackbarHostState()
    var filter by mutableStateOf(MemoFilter())

    fun go(screen: Screen) {
        stack.add(screen)
    }

    /** Replaces the current screen (e.g. a deleted memo's). */
    fun replace(screen: Screen) {
        stack[stack.lastIndex] = screen
    }

    /** False when there is nothing to go back to: the app closes. */
    fun back(): Boolean {
        if (stack.size <= 1) return false
        stack.removeAt(stack.lastIndex)
        return true
    }

    fun home() {
        stack.clear()
        stack.add(Screen.List)
    }

    fun notify(notice: Notice) {
        scope.launch {
            val action = notice.memoId?.let { "Open" }
            val duration = if (notice.tone == Tone.Info) SnackbarDuration.Short else SnackbarDuration.Long
            val result = snackbar.showSnackbar(notice.text, actionLabel = action, withDismissAction = true, duration = duration)
            if (result == SnackbarResult.ActionPerformed && notice.memoId != null) go(Screen.Memo(notice.memoId))
        }
    }

    fun error(e: Throwable, overrides: Map<String, String> = emptyMap()) {
        notify(Notice(Tone.Danger, errorText(e, overrides)))
    }

    fun info(text: String) = notify(Notice(Tone.Info, text))

    /** Runs a backend call, showing its error. */
    fun run(block: suspend () -> Unit) {
        scope.launch {
            try {
                block()
            } catch (e: BackendException) {
                error(e)
            }
        }
    }
}

private val Light =
    lightColorScheme(
        primary = Color(0xFF2E5E8C),
        secondary = Color(0xFF5B6B7C),
        tertiary = Color(0xFF8A5A00),
        background = Color(0xFFFBFBFA),
        surface = Color(0xFFFBFBFA),
    )

private val Dark =
    darkColorScheme(
        primary = Color(0xFF9CC4EE),
        secondary = Color(0xFFB4C3D3),
        tertiary = Color(0xFFF0C070),
        background = Color(0xFF16181B),
        surface = Color(0xFF16181B),
    )

@Composable
fun App(backend: Backend, platform: Platform, state: (AppState) -> Unit = {}) {
    val scope = rememberCoroutineScope()
    val app = remember { AppState(backend, platform, scope).also(state) }
    val session by backend.session.collectAsState()

    LaunchedEffect(backend) {
        backend.events.collect { events -> events.mapNotNull(::describe).forEach(app::notify) }
    }
    LaunchedEffect(session::class) {
        if (session !is Session.Unlocked) app.home()
    }

    MaterialTheme(colorScheme = if (isSystemInDarkTheme()) Dark else Light) {
        Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
            Box(Modifier.fillMaxSize()) {
                when (val s = session) {
                    Session.Starting -> CircularProgressIndicator(Modifier.align(Alignment.Center))
                    Session.SignedOut -> AuthScreen(app)
                    is Session.Onboarding -> RecoveryKeyScreen(app, s.recoveryKey)
                    is Session.Unlocked ->
                        when (val screen = app.screen) {
                            Screen.List -> ListScreen(app)
                            is Screen.Memo -> key(screen) { MemoScreen(app, screen.id, screen.startEditing) }
                            Screen.Settings -> SettingsScreen(app, s)
                        }
                }
                SnackbarHost(app.snackbar, Modifier.align(Alignment.BottomCenter).windowInsetsPadding(WindowInsets.safeDrawing))
            }
        }
    }
}
