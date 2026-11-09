package memo.app

import java.io.IOException
import java.net.HttpURLConnection
import java.net.URI
import memo.ui.BackendException
import org.json.JSONObject

private const val CLIENT = "android/0.1.0"

/** The API at `server` (`docs/api.md`). Blocking: call it from an IO dispatcher. */
class Http(server: String, var token: String? = null) {
    val server = normalize(server)
    private val base = "${this.server}/api/v1"

    class Response(val status: Int, val body: ByteArray) {
        val text get() = String(body, Charsets.UTF_8)

        /** The body as JSON; a body that is not JSON means this is not a memo server. */
        val json: JSONObject
            get() =
                try {
                    JSONObject(text)
                } catch (e: org.json.JSONException) {
                    throw BackendException("bad_server", "The server's answer is not what memo expects.")
                }
    }

    fun call(method: String, path: String, body: ByteArray? = null, type: String = "application/json"): Response {
        val connection =
            try {
                URI("$base$path").toURL().openConnection() as HttpURLConnection
            } catch (e: Exception) {
                throw BackendException("bad_server", "This is not a valid server address.")
            }
        try {
            connection.requestMethod = method
            connection.connectTimeout = 15_000
            connection.readTimeout = 60_000
            connection.useCaches = false
            connection.instanceFollowRedirects = false
            connection.setRequestProperty("Memo-Client", CLIENT)
            connection.setRequestProperty("Accept", "application/json")
            token?.let { connection.setRequestProperty("Authorization", "Bearer $it") }
            if (body != null) {
                connection.doOutput = true
                connection.setRequestProperty("Content-Type", type)
                connection.setFixedLengthStreamingMode(body.size)
                connection.outputStream.use { it.write(body) }
            }
            val status = connection.responseCode
            val stream = if (status >= 400) connection.errorStream else connection.inputStream
            val bytes = stream?.use { it.readBytes() } ?: ByteArray(0)
            if (status >= 300) throw error(status, bytes, connection.responseMessage)
            return Response(status, bytes)
        } catch (e: IOException) {
            throw BackendException("network", "The server cannot be reached.")
        } finally {
            connection.disconnect()
        }
    }

    fun json(method: String, path: String, body: JSONObject? = null): Response =
        call(method, path, body?.toString()?.toByteArray())

    private fun error(status: Int, body: ByteArray, reason: String?): BackendException {
        try {
            val error = JSONObject(String(body, Charsets.UTF_8)).getJSONObject("error")
            return BackendException(error.getString("code"), error.getString("message"), status)
        } catch (e: org.json.JSONException) {
            // Not an API error body (e.g. from the reverse proxy).
            return BackendException("http_$status", reason ?: "HTTP $status", status)
        }
    }

    companion object {
        /** `memo.example.org` → `https://memo.example.org`, without a trailing slash. */
        fun normalize(server: String): String {
            val trimmed = server.trim().trimEnd('/')
            return if (trimmed.contains("://")) trimmed else "https://$trimmed"
        }
    }
}
