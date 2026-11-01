// SPDX-License-Identifier: AGPL-3.0-or-later
import java.io.IOException
import java.net.URI
import java.security.MessageDigest
import java.util.Base64

plugins {
  id("com.android.application") version "9.4.1" apply false
  id("com.android.library") version "9.4.1" apply false
  id("org.jetbrains.kotlin.jvm") version "2.4.21" apply false
}

// The toolchain pin (tools/build/toolchain.env), from mise's environment.
fun pin(name: String): String =
  providers.environmentVariable(name).orNull ?: error("$name is not set (run Gradle through mise)")

val androidHome = pin("ANDROID_HOME")
val compileSdk = pin("ANDROID_COMPILE_SDK").toInt()
val buildTools = "$androidHome/build-tools/${pin("ANDROID_BUILD_TOOLS")}"

allprojects {
  layout.buildDirectory = rootDir.resolve("../../out/build/system-app/$name").normalize()
}

subprojects {
  extra["compileSdk"] = compileSdk
  extra["buildTools"] = buildTools
  // For the plain JVM modules. SDK 37 installs as platforms/android-37.0.
  extra["androidJar"] = "$androidHome/platforms/android-$compileSdk.0/android.jar"
}

// AOSP's public test-keys platform key signs the CI image
// (build/make/target/product/security/platform.*). The build fetches it once
// from googlesource, checks its SHA-256, and caches it outside the repo.
val platformKey =
  mapOf(
    "pk8" to "1ad8ef556870edb70f69a9d3c112544c07de5162ba440d84d33f8bb0c5962875",
    "x509.pem" to "9837de028f460c35cc8d3fa45f14eecce30f6fbfe4b93d399aef1acb80c20d14",
  )
val keyDir =
  File(
    System.getenv("XDG_CACHE_HOME") ?: "${System.getProperty("user.home")}/.cache",
    "innkeeper-dev/keys",
  )

val fetchPlatformKey =
  tasks.register("fetchPlatformKey") {
    val dir = keyDir
    val sums = platformKey
    outputs.files(sums.keys.map { dir.resolve("platform.$it") })
    doLast {
      fun sha256(b: ByteArray) =
        MessageDigest.getInstance("SHA-256").digest(b).joinToString("") { "%02x".format(it) }
      fun fetch(url: String): ByteArray {
        var attempt = 1
        while (true) {
          try {
            return Base64.getMimeDecoder().decode(URI(url).toURL().readText())
          } catch (e: IOException) {
            if (attempt == 3) throw e
            logger.warn("Platform key fetch failed (attempt $attempt/3), retrying: ${e.message}")
            Thread.sleep(attempt * 1000L)
            attempt++
          }
        }
      }
      dir.mkdirs()
      for ((ext, sum) in sums) {
        val f = dir.resolve("platform.$ext")
        if (f.isFile && sha256(f.readBytes()) == sum) continue
        val url =
          "https://android.googlesource.com/platform/build/+/refs/tags/android-17.0.0_r1/" +
            "target/product/security/platform.$ext?format=TEXT"
        val b = fetch(url)
        check(sha256(b) == sum) { "platform.$ext from googlesource: checksum mismatch" }
        f.writeBytes(b)
      }
    }
  }

// Every APK module's `signRelease` signs its release APK with the platform key (AGP leaves it
// unsigned), as signed/<module>.apk.
subprojects {
  pluginManager.withPlugin("com.android.application") {
    val module = project.name
    tasks.register<Exec>("signRelease") {
      dependsOn("assembleRelease", fetchPlatformKey)
      val unsigned = layout.buildDirectory.file("outputs/apk/release/$module-release-unsigned.apk")
      val signed = layout.buildDirectory.file("signed/$module.apk")
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
  }
}
