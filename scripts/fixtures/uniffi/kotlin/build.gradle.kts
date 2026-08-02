plugins {
    kotlin("jvm") version "2.0.21"
    application
}

// Compile-and-run fixture: proves the generated Kotlin UniFFI binding compiles
// against the JNA runtime and completes a live closed loop against a real
// relay. The generated sources are copied into build/generated-binding by
// scripts/build-ffi.sh.
sourceSets {
    main {
        kotlin.srcDir("build/generated-binding")
    }
}

dependencies {
    implementation("net.java.dev.jna:jna:5.14.0")
}

application {
    mainClass.set("ClosedLoopKt")
}

// scripts/build-ffi.sh runs this with -PrelayUrl=... -PffiLibraryPath=...
tasks.register<JavaExec>("ffiClosedLoop") {
    group = "verification"
    description = "Runs the Kotlin/JNA FFI closed loop against a live relay."
    mainClass.set("ClosedLoopKt")
    classpath = sourceSets.main.get().runtimeClasspath
    systemProperty("jna.library.path", providers.gradleProperty("ffiLibraryPath").orElse("").get())
    args = listOf(providers.gradleProperty("relayUrl").orElse("").get())
}
