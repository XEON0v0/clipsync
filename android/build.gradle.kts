plugins {
    id("com.android.application") version "8.8.2" apply false
    id("org.jetbrains.kotlin.android") version "2.2.21" apply false
    id("org.jetbrains.kotlin.plugin.compose") version "2.2.21" apply false
}

providers.environmentVariable("CLIPSYNC_ANDROID_BUILD_ROOT").orNull?.let { externalRoot ->
    layout.buildDirectory.set(file("$externalRoot/root"))
    subprojects {
        layout.buildDirectory.set(file("$externalRoot/$name"))
    }
}
