// SPDX-License-Identifier: AGPL-3.0-or-later
// InnkeeperProbes.apk (dev.sargunv.innkeeper.probes) holds diagnostic activities and the input
// harness for the end-to-end and compat tests. It is platform-signed like the system app but is
// not staged. `:probes:install` puts it in out/test/probes, and the tests install it into their
// session.
plugins { id("com.android.application") }

val sdk = extra["compileSdk"] as Int
val buildTools = extra["buildTools"] as String

android {
  namespace = "dev.sargunv.innkeeper.probes"
  compileSdk = sdk
  buildToolsVersion = File(buildTools).name

  defaultConfig {
    applicationId = "dev.sargunv.innkeeper.probes"
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

dependencies { implementation(project(":inject")) }

tasks.register<Sync>("install") {
  description = "Signs the release APK and installs it into out/test/probes."
  into(rootDir.resolve("../../out/test/probes"))
  from(tasks.named("signRelease")) { rename { "InnkeeperProbes.apk" } }
}
