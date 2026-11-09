package memo.app

import android.graphics.Bitmap
import android.graphics.Color
import android.graphics.ImageDecoder
import java.io.ByteArrayOutputStream
import java.nio.ByteBuffer
import kotlin.math.max
import kotlin.math.roundToInt
import memo.ui.BackendException

// Images before encryption (`docs/crypto.md#attachments`), as web/src/lib/images.ts does them:
// memo-core checks the type and strips metadata; Android applies EXIF rotation, downscales,
// converts HEIC and makes thumbnails.

private const val THUMB_SIDE = 512
/** Below this, the original doubles as its own thumbnail. */
private const val THUMB_MIN_BYTES = 256 * 1024
const val MAX_IMAGE = 25 * 1024 * 1024
const val DOWNSCALE_SIDE = 2560

class Encoded(val mime: String, val width: Int, val height: Int, val bytes: ByteArray)

class Prepared(val image: Encoded, val thumb: Encoded?)

private class Cleaned(val encoded: Encoded, val orientation: Int)

private fun clean(bytes: ByteArray): Cleaned {
    val reply = Core.call("clean_image", data = bytes)
    val meta = reply.obj
    return Cleaned(
        Encoded(meta.getString("mime"), meta.getInt("width"), meta.getInt("height"), reply.data ?: ByteArray(0)),
        meta.getInt("orientation"),
    )
}

private fun fit(width: Int, height: Int, side: Int): Pair<Int, Int> {
    val scale = minOf(1.0, side.toDouble() / max(width, height))
    return max(1, (width * scale).roundToInt()) to max(1, (height * scale).roundToInt())
}

/** Decodes with the EXIF rotation applied, at most `side` pixels on the longest side. */
private fun decode(bytes: ByteArray, side: Int?): Bitmap =
    try {
        ImageDecoder.decodeBitmap(ImageDecoder.createSource(ByteBuffer.wrap(bytes))) { decoder, info, _ ->
            decoder.allocator = ImageDecoder.ALLOCATOR_SOFTWARE
            if (side != null) {
                val (w, h) = fit(info.size.width, info.size.height, side)
                decoder.setTargetSize(w, h)
            }
        }
    } catch (e: Exception) {
        throw BackendException("bad_image", "This file is not a supported image.")
    }

private fun encode(bitmap: Bitmap, mime: String, quality: Int): ByteArray {
    val format =
        when (mime) {
            "image/png" -> Bitmap.CompressFormat.PNG
            "image/webp" -> Bitmap.CompressFormat.WEBP_LOSSY
            else -> Bitmap.CompressFormat.JPEG
        }
    val source =
        if (format == Bitmap.CompressFormat.JPEG && bitmap.hasAlpha()) {
            // JPEG has no transparency: put it on white, as the web client does.
            Bitmap.createBitmap(bitmap.width, bitmap.height, Bitmap.Config.ARGB_8888).also {
                android.graphics.Canvas(it).apply {
                    drawColor(Color.WHITE)
                    drawBitmap(bitmap, 0f, 0f, null)
                }
            }
        } else {
            bitmap
        }
    return ByteArrayOutputStream().use {
        source.compress(format, quality, it)
        it.toByteArray()
    }
}

/** `maxSide` downscales larger images (null: keep the size). GIFs are kept as they are. */
fun prepareImage(input: ByteArray, maxSide: Int?): Prepared {
    if (input.size > MAX_IMAGE) throw BackendException("too_large", "Images can be at most 25 MB.")
    val original =
        try {
            clean(input)
        } catch (e: BackendException) {
            // HEIC and other formats the platform decodes become JPEG.
            if (e.code != "malformed") throw e
            clean(encode(decode(input, maxSide), "image/jpeg", 90))
        }
    var image = original.encoded
    var bitmap: Bitmap? = null
    try {
        val tooLarge = maxSide != null && max(image.width, image.height) > maxSide
        if (image.mime != "image/gif" && (original.orientation != 1 || tooLarge)) {
            // Re-encoding applies the rotation and drops all metadata.
            bitmap = decode(input, maxSide)
            image = clean(encode(bitmap, image.mime, 90)).encoded
        }
        var thumb: Encoded? = null
        if (max(image.width, image.height) > THUMB_SIDE || image.bytes.size > THUMB_MIN_BYTES) {
            val small = decode(image.bytes, THUMB_SIDE)
            thumb = clean(encode(small, "image/webp", 80)).encoded
            small.recycle()
        }
        return Prepared(image, thumb)
    } finally {
        bitmap?.recycle()
    }
}
