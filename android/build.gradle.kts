plugins {
    alias(libs.plugins.android.application) apply false
    alias(libs.plugins.android.kmp.library) apply false
    alias(libs.plugins.kotlin.multiplatform) apply false
    alias(libs.plugins.kotlin.compose) apply false
}

// Every resolved version, transitive ones included, is pinned in a gradle.lockfile.
// Update on purpose: ./gradlew dependencies --write-locks, then scripts/maven-ages.py.
buildscript {
    configurations.classpath { resolutionStrategy.activateDependencyLocking() }
}
allprojects {
    dependencyLocking { lockAllConfigurations() }
}
