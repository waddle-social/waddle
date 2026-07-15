package social.waddle.android

import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory

/**
 * Ad-hoc [ViewModelProvider.Factory] over a single constructor lambda —
 * the manual-DI replacement for Hilt's ViewModel injection. Screens pass
 * `AppGraph`-backed dependencies through the closure.
 */
inline fun <reified VM : ViewModel> viewModelFactoryOf(
    crossinline create: () -> VM,
): ViewModelProvider.Factory = viewModelFactory {
    initializer { create() }
}
