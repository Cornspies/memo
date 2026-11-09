package memo.app

/** `crates/android`: one call, JSON request bytes in, encoded reply out. */
object Native {
    init {
        System.loadLibrary("memo")
    }

    @JvmStatic
    external fun call(request: ByteArray, data: ByteArray?): ByteArray
}
