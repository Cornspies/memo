package memo.app

import memo.ui.BackendException
import org.json.JSONObject
import org.json.JSONTokener

/** A reply from crates/native: JSON (a JSONObject, JSONArray, String, Number, Boolean or null) and bytes. */
class Reply(private val value: Any?, val data: ByteArray?) {
    val obj get() = value as JSONObject
    val array get() = value as org.json.JSONArray
    val string get() = value as String
    val bool get() = value as Boolean
    val isNull get() = value == null || value == JSONObject.NULL
    val raw get() = value
}

/**
 * Typed access to `Native.call`. Calls block (Argon2 takes about a second), so they only run on
 * [AndroidBackend]'s core dispatcher, one at a time.
 */
object Core {
    fun call(name: String, vararg args: Pair<String, Any?>, data: ByteArray? = null): Reply {
        val request = JSONObject().put("call", name)
        for ((key, value) in args) request.put(key, value ?: JSONObject.NULL)
        val out = Native.call(request.toString().toByteArray(), data)
        val len = ((out[1].toInt() and 0xff) shl 24) or ((out[2].toInt() and 0xff) shl 16) or
            ((out[3].toInt() and 0xff) shl 8) or (out[4].toInt() and 0xff)
        val value = JSONTokener(String(out, 5, len, Charsets.UTF_8)).nextValue()
        if (out[0].toInt() != 0) {
            val error = value as JSONObject
            throw BackendException(error.getString("code"), error.getString("message"))
        }
        val rest = if (out.size > 5 + len) out.copyOfRange(5 + len, out.size) else null
        return Reply(value, rest)
    }
}
