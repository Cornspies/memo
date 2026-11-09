package memo.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch

private enum class Mode(val title: String, val action: String) {
    Login("Sign in", "Sign in"),
    Register("Create your account", "Create account"),
    Recover("Recover your account", "Recover"),
}

@Composable
private fun Page(title: String, content: @Composable () -> Unit) {
    Column(
        Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing).imePadding()
            .verticalScroll(rememberScrollState()).padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Column(Modifier.widthIn(max = 480.dp).fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text("memo", style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.primary)
            Text(title, style = MaterialTheme.typography.headlineSmall)
            content()
        }
    }
}

@Composable
private fun Field(
    label: String,
    value: String,
    onChange: (String) -> Unit,
    secret: Boolean = false,
    keyboard: KeyboardType = KeyboardType.Text,
    mono: Boolean = false,
) {
    OutlinedTextField(
        value,
        onChange,
        label = { Text(label) },
        singleLine = true,
        visualTransformation = if (secret) PasswordVisualTransformation() else androidx.compose.ui.text.input.VisualTransformation.None,
        keyboardOptions =
            KeyboardOptions(
                capitalization = KeyboardCapitalization.None,
                autoCorrectEnabled = false,
                keyboardType = if (secret) KeyboardType.Password else keyboard,
            ),
        textStyle = if (mono) MaterialTheme.typography.bodyLarge.copy(fontFamily = FontFamily.Monospace) else MaterialTheme.typography.bodyLarge,
        modifier = Modifier.fillMaxWidth(),
    )
}

@Composable
fun AuthScreen(app: AppState) {
    val scope = rememberCoroutineScope()
    var mode by remember { mutableStateOf(Mode.Login) }
    var server by remember { mutableStateOf("") }
    var username by remember { mutableStateOf("") }
    var password by remember { mutableStateOf("") }
    var confirm by remember { mutableStateOf("") }
    var invite by remember { mutableStateOf("") }
    var recoveryKey by remember { mutableStateOf("") }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }

    fun submit() {
        error = null
        if (mode != Mode.Login) {
            app.backend.passwordProblem(password)?.let {
                error = it
                return
            }
            if (password != confirm) {
                error = "The passwords do not match."
                return
            }
        }
        busy = true
        scope.launch {
            try {
                when (mode) {
                    Mode.Login -> app.backend.login(server, username, password)
                    Mode.Register -> app.backend.register(server, username, password, invite)
                    Mode.Recover -> app.backend.recover(server, username, recoveryKey, password)
                }
            } catch (e: BackendException) {
                error =
                    errorText(
                        e,
                        mapOf(
                            "conflict" to "This username is taken.",
                            "bad_request" to
                                if (mode == Mode.Register) "Usernames are 3–32 characters: a–z, 0–9, _ . -" else "",
                            "invalid_credentials" to
                                if (mode == Mode.Recover) {
                                    "Wrong username or recovery key."
                                } else {
                                    "Wrong username or password."
                                },
                        ),
                    )
            } finally {
                busy = false
            }
        }
    }

    Page(mode.title) {
        Field("Server", server, { server = it }, keyboard = KeyboardType.Uri)
        Field("Username", username, { username = it })
        if (mode == Mode.Recover) Field("Recovery key", recoveryKey, { recoveryKey = it }, mono = true)
        Field(if (mode == Mode.Recover) "New password" else "Password", password, { password = it }, secret = true)
        if (mode != Mode.Login) Field("Repeat the password", confirm, { confirm = it }, secret = true)
        if (mode == Mode.Register) Field("Invite code", invite, { invite = it })
        if (mode != Mode.Login) {
            Text(
                "At least 12 characters. Your password encrypts your notes: nobody can reset it for you.",
                style = MaterialTheme.typography.bodySmall,
            )
        }
        error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
        Button(
            ::submit,
            enabled = !busy && server.isNotBlank() && username.isNotBlank() && password.isNotEmpty(),
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text(if (busy) "Working…" else mode.action)
        }
        FlowRow(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
            for (other in Mode.entries.filter { it != mode }) {
                TextButton(
                    {
                        mode = other
                        error = null
                        password = ""
                        confirm = ""
                    },
                ) {
                    Text(
                        when (other) {
                            Mode.Login -> "I have an account"
                            Mode.Register -> "Create an account"
                            Mode.Recover -> "Forgot your password?"
                        },
                    )
                }
            }
        }
    }
}

@Composable
fun RecoveryKeyScreen(app: AppState, key: String) {
    val scope = rememberCoroutineScope()
    val groups = remember(key) { key.split("-") }
    var typed by remember { mutableStateOf("") }
    var copied by remember { mutableStateOf(false) }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }

    Page("Save your recovery key") {
        Text(
            "If you forget your password, this key is the only way back into your account. Nobody else can " +
                "reset it, not even the server's admin. Write it down or store it in a password manager.",
        )
        FlowRow(horizontalArrangement = Arrangement.spacedBy(10.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            for (group in groups) {
                Text(group, fontFamily = FontFamily.Monospace, style = MaterialTheme.typography.titleMedium)
            }
        }
        Row {
            TextButton({
                app.platform.copy(key)
                copied = true
            }) { Text(if (copied) "Copied" else "Copy") }
        }
        Field("To confirm, type the last group (${groups.size}th)", typed, { typed = it }, mono = true)
        error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
        Button(
            {
                busy = true
                scope.launch {
                    try {
                        app.backend.finishOnboarding()
                    } catch (e: BackendException) {
                        error = errorText(e)
                        busy = false
                    }
                }
            },
            enabled = !busy && typed.trim().uppercase() == groups.last(),
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text(if (busy) "Starting…" else "I saved it, continue")
        }
    }
}
