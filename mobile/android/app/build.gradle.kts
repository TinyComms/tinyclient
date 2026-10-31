plugins {
    id("com.android.application")
}

android {
    namespace = "dev.tinyclient.app"
    // Play requires targetSdk 36 for new apps and updates since 2026-08-31; compileSdk 36 is the
    // matching platform. API 37 needs AGP >= 9.1.1, so it is a later move.
    compileSdk = 36
    // NDK r30 LTS (30.0.16248370). r28+ emits 16 KB-aligned ELF segments by default, which is
    // what the 2027-02-01 Play requirement (16 KB pages, updates blocked without it) asks for.
    // cargo-ndk adds the same flags for older NDKs; the ELF still has to be verified, see
    // docs/mobile-build.md.
    ndkVersion = "30.0.16248370"

    defaultConfig {
        applicationId = "dev.tinyclient.app"
        // API 29 (Android 10): scoped storage, and the first release with the exec restriction
        // that makes an in-process core mandatory. Nothing here needs 30+.
        minSdk = 29
        targetSdk = 36
        versionCode = 1
        versionName = "0.2.15"
        // The library is built by scripts/mobile/build-android.ps1 into src/main/jniLibs.
        // arm64-v8a is mandatory for Play; x86_64 is for the emulator.
        ndk {
            abiFilters += listOf("arm64-v8a", "x86_64")
        }
    }

    buildTypes {
        release {
            // No Kotlin/Java to shrink yet, and R8 would rename nothing we reflect over. Turn
            // this on with the first Kotlin activity, and put a minified build in CI the same
            // day (research_notes/mobile/rust-core-native-shells.md §3.5).
            isMinifyEnabled = false
        }
    }

    // Uncompressed, page-aligned native libraries inside the APK. This is the APK half of 16 KB
    // support (AGP 8.5.1+); the ELF half comes from the NDK. `zipalign -c -P 16` verifies it.
    packaging {
        jniLibs {
            useLegacyPackaging = false
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}
