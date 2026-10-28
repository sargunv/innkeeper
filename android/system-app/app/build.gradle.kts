// SPDX-License-Identifier: AGPL-3.0-or-later
// "Linux.apk System": the privileged system app. `:app:install` signs the
// release APK with the platform key (AGP leaves it unsigned) and installs it
// into out/build/system-app/install, the module's tree of the stage prefix.
import java.net.URI
import java.security.MessageDigest
import java.util.Base64

plugins { id("com.android.application") }

val sdk = extra["compileSdk"] as Int
val buildTools = extra["buildTools"] as String

android {
  namespace = "dev.linuxapk.system"
  compileSdk = sdk
  buildToolsVersion = File(buildTools).name

  defaultConfig {
    applicationId = "dev.linuxapk.system"
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
  compileOnly(project(":hidden-api"))
}

// AOSP's public test-keys platform key, which signs the CI image
// (build/make/target/product/security/platform.*): fetched once from
// googlesource, checked by SHA-256, and cached outside the repo.
val platformKey =
  mapOf(
    "pk8" to "1ad8ef556870edb70f69a9d3c112544c07de5162ba440d84d33f8bb0c5962875",
    "x509.pem" to "9837de028f460c35cc8d3fa45f14eecce30f6fbfe4b93d399aef1acb80c20d14",
  )
val keyDir =
  File(
    System.getenv("XDG_CACHE_HOME") ?: "${System.getProperty("user.home")}/.cache",
    "linuxapk-dev/keys",
  )

val fetchPlatformKey =
  tasks.register("fetchPlatformKey") {
    val dir = keyDir
    val sums = platformKey
    outputs.files(sums.keys.map { dir.resolve("platform.$it") })
    doLast {
      fun sha256(b: ByteArray) =
        MessageDigest.getInstance("SHA-256").digest(b).joinToString("") { "%02x".format(it) }
      dir.mkdirs()
      for ((ext, sum) in sums) {
        val f = dir.resolve("platform.$ext")
        if (f.isFile && sha256(f.readBytes()) == sum) continue
        val url =
          "https://android.googlesource.com/platform/build/+/refs/tags/android-17.0.0_r1/" +
            "target/product/security/platform.$ext?format=TEXT"
        val b = Base64.getMimeDecoder().decode(URI(url).toURL().readText())
        check(sha256(b) == sum) { "platform.$ext from googlesource: checksum mismatch" }
        f.writeBytes(b)
      }
    }
  }

val signRelease =
  tasks.register<Exec>("signRelease") {
    dependsOn("assembleRelease", fetchPlatformKey)
    val unsigned = layout.buildDirectory.file("outputs/apk/release/app-release-unsigned.apk")
    val signed = layout.buildDirectory.file("signed/LinuxApkSystem.apk")
    inputs.file(unsigned)
    outputs.file(signed)
    commandLine(
      "$buildTools/apksigner",
      "-J-enable-native-access=ALL-UNNAMED",
      "sign",
      "--key",
      keyDir.resolve("platform.pk8"),
      "--cert",
      keyDir.resolve("platform.x509.pem"),
      "--v4-signing-enabled",
      "false",
      "--out",
      signed.get().asFile,
      unsigned.get().asFile,
    )
  }

tasks.register<Sync>("install") {
  description = "Signs the release APK and installs it into out/build/system-app/install."
  into(layout.buildDirectory.dir("../install"))
  val systemExt = "share/linuxapk/android/system_ext"
  from(signRelease) { into("$systemExt/priv-app/LinuxApkSystem") }
  from(rootDir.resolve("etc/permissions")) { into("$systemExt/etc/permissions") }
}
