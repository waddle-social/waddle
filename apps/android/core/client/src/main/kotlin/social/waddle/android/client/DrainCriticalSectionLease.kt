package social.waddle.android.client

/** Opaque, one-use ownership of the lifecycle store's drain critical section. */
internal class DrainCriticalSectionLease private constructor() {
    companion object {
        fun issue(): DrainCriticalSectionLease = DrainCriticalSectionLease()
    }
}
