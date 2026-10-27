// SPDX-License-Identifier: AGPL-3.0-or-later
// "Linux.apk System": the privileged system app. Signed with the platform key
// by android/build.sh (AGP leaves the release APK unsigned).
plugins { id("com.android.application") }

android {
  namespace = "dev.linuxapk.system"
  compileSdk = 37
  buildToolsVersion = "37.0.0"

  defaultConfig {
    applicationId = "dev.linuxapk.system"
    minSdk = 37
    targetSdk = 37
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
  compileOnly(project(":hidden-api"))
}
