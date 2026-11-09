package memo.app

import android.content.Context
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import java.util.concurrent.TimeUnit

/** Sync in the background (`docs/sync.md#when-sync-runs`): hourly, and once when the network returns. */
class SyncWorker(context: Context, params: WorkerParameters) : CoroutineWorker(context, params) {
    override suspend fun doWork(): Result {
        val backend = (applicationContext as MemoApp).backend
        return if (backend.syncInBackground()) Result.success() else Result.retry()
    }

    companion object {
        private const val PERIODIC = "sync"
        private const val ONCE = "sync-once"

        private val online = Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build()

        /** On leaving the app. `soon`: there are local changes, so also sync as soon as there is a network. */
        fun schedule(context: Context, soon: Boolean) {
            val work = WorkManager.getInstance(context)
            work.enqueueUniquePeriodicWork(
                PERIODIC,
                ExistingPeriodicWorkPolicy.KEEP,
                PeriodicWorkRequestBuilder<SyncWorker>(1, TimeUnit.HOURS).setConstraints(online).build(),
            )
            if (soon) {
                work.enqueueUniqueWork(
                    ONCE,
                    ExistingWorkPolicy.REPLACE,
                    OneTimeWorkRequestBuilder<SyncWorker>().setConstraints(online).build(),
                )
            }
        }

        fun cancel(context: Context) {
            val work = WorkManager.getInstance(context)
            work.cancelUniqueWork(PERIODIC)
            work.cancelUniqueWork(ONCE)
        }
    }
}
