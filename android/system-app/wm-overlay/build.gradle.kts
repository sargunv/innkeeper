// SPDX-License-Identifier: AGPL-3.0-or-later
plugins { id("com.android.application") }

val sdk = extra["compileSdk"] as Int
val buildTools = extra["buildTools"] as String

// A resource-only overlay must not package the default Kotlin runtime.
configurations
  .matching { it.name.endsWith("RuntimeClasspath") }
  .configureEach {
    exclude(group = "org.jetbrains.kotlin", module = "kotlin-stdlib")
  }

android {
  namespace = "dev.linuxapk.windowpolicy"
  compileSdk = sdk
  buildToolsVersion = File(buildTools).name
  defaultConfig {
    applicationId = "dev.linuxapk.windowpolicy"
    minSdk = sdk
    targetSdk = sdk
    versionCode = 1
    versionName = "0.1"
  }
  buildTypes { release { signingConfig = null } }
}
