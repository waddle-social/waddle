package social.waddle.android.connection

import org.koin.dsl.module

internal val connectionModule = module {
    single { WaddleConnectionManager() }
}
