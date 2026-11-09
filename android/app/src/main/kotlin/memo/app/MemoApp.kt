package memo.app

import android.app.Application

/** One backend per process, shared by the activity and the background worker. */
class MemoApp : Application() {
    val backend by lazy { AndroidBackend(this) }
}
