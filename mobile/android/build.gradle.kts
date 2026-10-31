plugins {
    // AGP 9.5.0 (October 2026). AGP 9 removed the old variant API, so nothing here uses
    // `applicationVariants` / `libraryVariants`.
    id("com.android.application") version "9.5.0" apply false
    // Element X Android builds with Kotlin 2.4.21 (2026). No Kotlin source is compiled yet:
    // the shell is `android.app.NativeActivity` and every line of UI is the Rust library. The
    // plugin is declared so that adding a Kotlin activity later is a one-line change, and so
    // the version is pinned in one place.
    id("org.jetbrains.kotlin.android") version "2.4.21" apply false
}
