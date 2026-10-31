// SPDX-License-Identifier: AGPL-3.0-or-later
// Android input events from :protocol's translated ones, and their injection:
// shared by the system app and the probes' input harness.
plugins { id("com.android.library") }

val sdk = extra["compileSdk"] as Int

android {
  namespace = "dev.sargunv.innkeeper.system.input"
  compileSdk = sdk
  defaultConfig { minSdk = sdk }

  compileOptions {
    sourceCompatibility = JavaVersion.VERSION_17
    targetCompatibility = JavaVersion.VERSION_17
  }

  lint { abortOnError = false }
}

dependencies { api(project(":protocol")) }
