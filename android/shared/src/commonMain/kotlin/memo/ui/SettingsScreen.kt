package memo.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Checkbox
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Switch
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
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch

@Composable
private fun Section(title: String) {
    HorizontalDivider(Modifier.padding(top = 8.dp))
    Text(title, style = MaterialTheme.typography.titleMedium)
}

@Composable
private fun Muted(text: String) {
    Text(text, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
}

@Composable
private fun Password(label: String, value: String, onChange: (String) -> Unit) {
    OutlinedTextField(
        value,
        onChange,
        label = { Text(label) },
        singleLine = true,
        visualTransformation = PasswordVisualTransformation(),
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
        modifier = Modifier.fillMaxWidth(),
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(app: AppState, session: Session.Unlocked) {
    val sync by app.backend.sync.collectAsState()
    var me by remember { mutableStateOf<Me?>(null) }
    var devices by remember { mutableStateOf<List<Device>?>(null) }
    var removing by remember { mutableStateOf<Device?>(null) }
    var downscale by remember { mutableStateOf(app.backend.downscale) }
    var current by remember { mutableStateOf("") }
    var next by remember { mutableStateOf("") }
    var repeat by remember { mutableStateOf("") }
    var revokeOthers by remember { mutableStateOf(false) }
    var passwordBusy by remember { mutableStateOf(false) }
    var passwordError by remember { mutableStateOf<String?>(null) }
    var exporting by remember { mutableStateOf(false) }
    var signingOut by remember { mutableStateOf(false) }
    var deleteName by remember { mutableStateOf("") }
    var deletePassword by remember { mutableStateOf("") }
    var deleteBusy by remember { mutableStateOf(false) }
    var deleteError by remember { mutableStateOf<String?>(null) }

    suspend fun load() {
        try {
            me = app.backend.me()
            devices = app.backend.devices()
        } catch (e: BackendException) {
            app.error(e)
        }
    }
    LaunchedEffect(Unit) { load() }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Settings") },
                navigationIcon = { IconButton({ app.back() }) { Icon(Icons.Back, "Back") } },
            )
        },
    ) { padding ->
        Column(
            Modifier.padding(padding).fillMaxSize().imePadding().verticalScroll(rememberScrollState()).padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Text("Account", style = MaterialTheme.typography.titleMedium)
            Text(
                "Signed in as ${session.username}" + (if (me?.role == "admin") " (admin)" else "") + " on ${session.server}" +
                    (me?.let { ", since ${app.platform.dateTime(it.createdAt)}." } ?: "."),
            )
            me?.let { Muted("Storage: ${size(it.usedBytes)} of ${size(it.quotaBytes)} used.") }

            Section("Your key fingerprint")
            Muted("People you share with can compare this with what their app shows for you, to be sure nobody is in between.")
            Text(session.fingerprint, fontFamily = FontFamily.Monospace)

            Section("Images")
            Row(verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text("Downscale large images")
                    Muted("Attach photos at most 2560 pixels wide. Saves space and data.")
                }
                Switch(downscale, {
                    downscale = it
                    app.backend.downscale = it
                })
            }

            Section("Devices")
            val list = devices
            if (list == null) {
                Muted("Loading…")
            } else {
                for (device in list) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Column(Modifier.weight(1f)) {
                            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                Text(device.name, fontWeight = FontWeight.SemiBold)
                                if (device.current) Badge("This device")
                            }
                            Muted("${device.kind} · last seen ${app.platform.relative(device.lastSeenAt)}")
                        }
                        if (!device.current) {
                            TextButton({ removing = device }) { Text("Sign out", color = MaterialTheme.colorScheme.error) }
                        }
                    }
                }
            }

            Section("Change password")
            passwordError?.let { Text(it, color = MaterialTheme.colorScheme.error) }
            Password("Current password", current) { current = it }
            Password("New password", next) { next = it }
            Password("Repeat the new password", repeat) { repeat = it }
            Row(verticalAlignment = Alignment.CenterVertically) {
                Checkbox(revokeOthers, { revokeOthers = it })
                Text("Sign out all other devices")
            }
            Button({
                passwordError = app.backend.passwordProblem(next) ?: if (next != repeat) "The passwords do not match." else null
                if (passwordError != null) return@Button
                passwordBusy = true
                app.scope.launch {
                    try {
                        app.backend.changePassword(current, next, revokeOthers)
                        current = ""
                        next = ""
                        repeat = ""
                        app.info("Password changed.")
                        load()
                    } catch (e: BackendException) {
                        passwordError = errorText(e, mapOf("invalid_credentials" to "The current password is wrong."))
                    } finally {
                        passwordBusy = false
                    }
                }
            }, enabled = !passwordBusy && current.isNotEmpty() && next.isNotEmpty()) {
                Text(if (passwordBusy) "Changing…" else "Change password")
            }

            Section("Export")
            Muted("All your memos as Markdown files with their images, in a ZIP file.")
            OutlinedButton({
                exporting = true
                app.scope.launch {
                    try {
                        val zip = app.backend.exportAll()
                        if (app.platform.saveFile("memo-export.zip", "application/zip", zip)) app.info("Exported.")
                    } catch (e: BackendException) {
                        app.error(e)
                    } finally {
                        exporting = false
                    }
                }
            }, enabled = !exporting) { Text(if (exporting) "Exporting…" else "Export memos") }

            Section("Sign out")
            Muted("Signing out removes your memos from this device. They stay on the server and your other devices.")
            if (sync.unsynced) {
                Text(
                    "Some changes are not synced yet. They are lost if you sign out now.",
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodySmall,
                )
            }
            OutlinedButton({ signingOut = true }) { Text("Sign out") }

            Section("Delete account")
            Text(
                "Deletes your account, every memo you own (also for the people you shared them with) and all your " +
                    "images. This cannot be undone.",
                style = MaterialTheme.typography.bodySmall,
            )
            deleteError?.let { Text(it, color = MaterialTheme.colorScheme.error) }
            OutlinedTextField(
                deleteName,
                { deleteName = it },
                label = { Text("Type your username to confirm") },
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )
            Password("Password", deletePassword) { deletePassword = it }
            Button(
                {
                    deleteBusy = true
                    deleteError = null
                    app.scope.launch {
                        try {
                            app.backend.deleteAccount(deletePassword)
                        } catch (e: BackendException) {
                            deleteError = errorText(e, mapOf("invalid_credentials" to "Wrong password."))
                            deleteBusy = false
                        }
                    }
                },
                enabled = !deleteBusy && deleteName.trim() == session.username && deletePassword.isNotEmpty(),
                colors = ButtonDefaults.buttonColors(containerColor = MaterialTheme.colorScheme.error),
            ) { Text(if (deleteBusy) "Deleting…" else "Delete my account") }
        }
    }

    removing?.let { device ->
        AlertDialog(
            onDismissRequest = { removing = null },
            title = { Text("Sign out ${device.name}?") },
            text = { Text("Unsynced changes on it are lost.") },
            confirmButton = {
                TextButton({
                    removing = null
                    app.run {
                        app.backend.removeDevice(device.id)
                        load()
                    }
                }) { Text("Sign out") }
            },
            dismissButton = { TextButton({ removing = null }) { Text("Cancel") } },
        )
    }
    if (signingOut) {
        AlertDialog(
            onDismissRequest = { signingOut = false },
            title = { Text("Sign out?") },
            text = {
                Text(
                    if (sync.unsynced) {
                        "Changes not synced yet are lost."
                    } else {
                        "Your memos are removed from this device."
                    },
                )
            },
            confirmButton = {
                TextButton({
                    signingOut = false
                    app.run { app.backend.logout() }
                }) { Text("Sign out") }
            },
            dismissButton = { TextButton({ signingOut = false }) { Text("Cancel") } },
        )
    }
}

fun size(bytes: Long): String =
    when {
        bytes < 1024 -> "$bytes B"
        bytes < 1024 * 1024 -> "${bytes / 1024} KB"
        bytes < 1024L * 1024 * 1024 -> "${(bytes * 10 / (1024 * 1024)) / 10.0} MB"
        else -> "${(bytes * 10 / (1024L * 1024 * 1024)) / 10.0} GB"
    }
