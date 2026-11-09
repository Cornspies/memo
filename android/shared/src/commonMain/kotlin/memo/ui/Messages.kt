package memo.ui

// The same wording as the web app (web/src/lib/state.svelte.ts).

enum class Tone { Info, Warning, Danger }

data class Notice(val tone: Tone, val text: String, val memoId: String? = null)

fun describe(event: SyncEvent): Notice? =
    when (event.type) {
        "reset" -> Notice(Tone.Info, "The server asked for a full resync. Everything is up to date again.")
        "conflict" ->
            Notice(Tone.Warning, "A memo was edited elsewhere at the same time. Choose what to keep.", event.memoId)
        "restored_copy" ->
            Notice(
                Tone.Warning,
                if (event.reason == "deleted") {
                    "A memo you edited was deleted elsewhere. Your edit was saved as a new memo."
                } else {
                    "You lost access to a memo you edited. Your edit was saved as a new private memo."
                },
                event.copy,
            )
        "delete_rejected" -> Notice(Tone.Info, "A memo was not deleted because it changed elsewhere.", event.memoId)
        "push_rejected" ->
            Notice(
                Tone.Danger,
                when (event.reason) {
                    "quota_exceeded" ->
                        "Your storage quota is full. Some changes stay on this device until you free up space."
                    "forbidden" -> "You can no longer edit a memo. Your changes are kept here; save them as a copy."
                    else -> "The server refused a change."
                },
                event.memoId,
            )
        "server_lost_data" ->
            Notice(
                Tone.Info,
                "The server had an older copy of a memo. This device's version was sent back.",
                event.memoId,
            )
        "cannot_decrypt" -> Notice(Tone.Danger, "A memo cannot be decrypted.", event.memoId)
        "newer_format" -> Notice(Tone.Warning, "A memo was saved by a newer app version. Update to edit it.", event.memoId)
        "key_rejected" ->
            Notice(
                Tone.Danger,
                "A shared memo's key did not verify against its owner's key. It was not opened.",
                event.memoId,
            )
        "key_changed" ->
            Notice(
                Tone.Danger,
                "The server presented a different key for one of your contacts. Check it before sharing.",
            )
        "pin_conflict" -> Notice(Tone.Danger, "Two of your devices saw different keys for the same contact. Verify it.")
        "vault_newer_format" -> Notice(Tone.Warning, "Your contact list was saved by a newer app version. Update the app.")
        "vault_undecryptable" -> Notice(Tone.Danger, "Your contact list cannot be decrypted.")
        "signed_out" ->
            Notice(Tone.Warning, "This device was signed out from another device. Its copy of your memos was removed.")
        else -> null
    }

private val MESSAGES =
    mapOf(
        "network" to "The server cannot be reached.",
        "invalid_credentials" to "Wrong username or password.",
        "invite_invalid" to "This invite code is unknown, expired or already used.",
        "rate_limited" to "Too many attempts. Wait a little and try again.",
        "recovery_key_invalid" to "This recovery key is not valid. Check it for typos.",
        "key_changed" to
            "This user's key differs from the one you saw before. Do not share until you have checked it with them.",
        "not_pinned" to "A member's key is not known on this device yet. Sync and try again.",
        "read_only" to "You cannot edit this memo.",
        "in_conflict" to "Resolve the conflict first.",
        "not_owner" to "Only the owner can do this.",
        "quota_exceeded" to "Your storage quota is full.",
        "too_large" to "This is too large.",
        "malformed" to "This file is not a supported image (JPEG, PNG, WebP or GIF).",
        "bad_image" to "This file is not a supported image (JPEG, PNG, WebP or GIF).",
        "decrypt" to "Decryption failed.",
        "kdf_params" to "The server sent unacceptable key derivation parameters.",
        "bad_server" to "This is not a memo server address.",
    )

fun errorText(error: Throwable, overrides: Map<String, String> = emptyMap()): String =
    if (error is BackendException) {
        overrides[error.code] ?: MESSAGES[error.code] ?: error.message ?: error.code
    } else {
        error.message ?: error.toString()
    }

fun errorCode(error: Throwable): String? = (error as? BackendException)?.code
