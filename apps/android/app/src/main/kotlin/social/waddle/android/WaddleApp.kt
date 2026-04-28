package social.waddle.android

import android.app.Application
import org.koin.android.ext.koin.androidContext
import org.koin.core.context.startKoin
import social.waddle.android.auth.authModule
import social.waddle.android.connection.connectionModule
import social.waddle.android.ffi.WaddleNativeLoader
import social.waddle.android.session.sessionModule

internal class WaddleApp : Application() {
    override fun onCreate() {
        super.onCreate()
        WaddleNativeLoader.load()

        startKoin {
            androidContext(this@WaddleApp)
            modules(authModule, connectionModule, sessionModule)
        }
    }
}
