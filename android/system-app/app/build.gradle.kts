// SPDX-License-Identifier: AGPL-3.0-or-later
// "Innkeeper System": the privileged system app. `:app:install` installs the
// platform-signed release APK (the root project's signRelease) into
// out/build/system-app/install, the module's tree of the stage prefix.
plugins { id("com.android.application") }

val sdk = extra["compileSdk"] as Int
val buildTools = extra["buildTools"] as String

android {
  namespace = "dev.sargunv.innkeeper.system"
  compileSdk = sdk
  buildToolsVersion = File(buildTools).name

  defaultConfig {
    applicationId = "dev.sargunv.innkeeper.system"
    minSdk = sdk
    targetSdk = sdk
    versionCode = 1
    versionName = "0.1"
  }

  buildTypes {
    release {
      isMinifyEnabled = false
      signingConfig = null
    }
  }

  compileOptions {
    sourceCompatibility = JavaVersion.VERSION_17
    targetCompatibility = JavaVersion.VERSION_17
  }

  lint { abortOnError = false }
}

dependencies {
  implementation(project(":protocol"))
  implementation(project(":inject"))
  compileOnly(project(":hidden-api"))
}

tasks.register<Sync>("install") {
  description = "Signs the release APK and installs it into out/build/system-app/install."
  into(layout.buildDirectory.dir("../install"))
  val systemExt = "share/innkeeper/android/system_ext"
  from(tasks.named("signRelease")) {
    into("$systemExt/priv-app/InnkeeperSystem")
    rename { "InnkeeperSystem.apk" }
  }
  from(project(":framework-overlay").tasks.named("signRelease")) {
    into("$systemExt/overlay")
    rename { "InnkeeperFrameworkOverlay.apk" }
  }
  from(project(":settings-overlay").tasks.named("signRelease")) {
    into("$systemExt/overlay")
    rename { "InnkeeperSettingsOverlay.apk" }
  }
  from(rootDir.resolve("etc/permissions")) { into("$systemExt/etc/permissions") }
}
