package social.waddle.android.session

import org.koin.dsl.module

/**
 * Reserved for the per-session graph (repositories built around an
 * authenticated [social.waddle.android.ffi.WaddleClientHandle]). Empty for
 * now — auth flow lands in the next pass.
 */
internal val sessionModule = module {
}
