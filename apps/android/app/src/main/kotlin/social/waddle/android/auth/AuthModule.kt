package social.waddle.android.auth

import org.koin.androidx.viewmodel.dsl.viewModel
import org.koin.dsl.module
import social.waddle.android.domain.auth.SessionStore
import social.waddle.android.domain.auth.WaddleAuthApi

internal val authModule = module {
    single { WaddleAuthApi() }
    single { SessionStore(get()) }
    viewModel { AuthViewModel(get(), get()) }
}
