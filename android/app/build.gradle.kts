plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
}

// The release key (README.md, Release build). Without it the release APK is unsigned.
val keystore = providers.environmentVariable("MEMO_KEYSTORE").orNull

android {
    namespace = "memo.app"
    compileSdk = 37
    // Only for stripping; the library itself comes from build-rust.sh.
    ndkVersion = "27.3.13750724"

    defaultConfig {
        applicationId = "memo.app"
        minSdk = 30
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"
    }

    if (keystore != null) {
        signingConfigs {
            create("release") {
                storeFile = file(keystore)
                storePassword = providers.environmentVariable("MEMO_KEYSTORE_PASSWORD").get()
                keyAlias = providers.environmentVariable("MEMO_KEY_ALIAS").get()
                keyPassword = providers.environmentVariable("MEMO_KEY_PASSWORD").get()
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.findByName("release")
        }
    }
}

// libmemo.so from crates/android, built by cargo before every build.
val buildRust = tasks.register<Exec>("buildRust") {
    workingDir = rootDir
    commandLine("sh", "build-rust.sh")
}
tasks.named("preBuild") { dependsOn(buildRust) }

dependencies {
    implementation(project(":shared"))
    implementation(libs.activity.compose)
    implementation(libs.work.runtime)
}
