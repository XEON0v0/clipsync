plugins {
    kotlin("jvm") version "2.0.21"
}

// Compile-only fixture: proves the generated Kotlin UniFFI binding compiles
// against the JNA runtime it targets. The generated sources are copied into
// build/generated-binding by scripts/spike-uniffi.sh.
sourceSets {
    main {
        kotlin.srcDir("build/generated-binding")
    }
}

dependencies {
    implementation("net.java.dev.jna:jna:5.14.0")
}
