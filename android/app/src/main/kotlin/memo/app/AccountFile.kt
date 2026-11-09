package memo.app

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import java.io.File
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import org.json.JSONObject

/**
 * The signed-in account: server, username, device token and the keys that unlock without the
 * password (`docs/crypto.md#account-flows`). Encrypted with an AES key that never leaves the
 * Android Keystore, in no-backup storage.
 */
data class Account(
    val server: String,
    val username: String,
    val userId: String,
    val token: String,
    /** The `secrets` memo-core returned when the account was opened. */
    val secrets: JSONObject,
)

class AccountFile(context: Context) {
    private val file = File(context.noBackupFilesDir, "account")

    fun read(): Account? {
        if (!file.exists()) return null
        return try {
            val bytes = file.readBytes()
            val cipher = Cipher.getInstance(TRANSFORMATION)
            cipher.init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(128, bytes, 0, IV_LEN))
            val json = JSONObject(String(cipher.doFinal(bytes, IV_LEN, bytes.size - IV_LEN), Charsets.UTF_8))
            Account(
                json.getString("server"),
                json.getString("username"),
                json.getString("user_id"),
                json.getString("token"),
                json.getJSONObject("secrets"),
            )
        } catch (e: Exception) {
            // The Keystore key is gone (e.g. the device was reset): sign in again.
            null
        }
    }

    fun write(account: Account) {
        val json =
            JSONObject()
                .put("server", account.server)
                .put("username", account.username)
                .put("user_id", account.userId)
                .put("token", account.token)
                .put("secrets", account.secrets)
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, key())
        val sealed = cipher.iv + cipher.doFinal(json.toString().toByteArray())
        val tmp = File(file.path + ".tmp")
        tmp.writeBytes(sealed)
        tmp.renameTo(file)
    }

    fun delete() {
        file.delete()
    }

    private fun key(): SecretKey {
        val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (store.getKey(ALIAS, null) as? SecretKey)?.let { return it }
        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
        generator.init(
            KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .build(),
        )
        return generator.generateKey()
    }

    private companion object {
        const val ALIAS = "memo-account"
        const val TRANSFORMATION = "AES/GCM/NoPadding"
        const val IV_LEN = 12
    }
}
