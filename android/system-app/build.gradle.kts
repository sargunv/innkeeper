// SPDX-License-Identifier: AGPL-3.0-or-later
plugins {
  id("com.android.application") version "9.4.1" apply false
  id("org.jetbrains.kotlin.jvm") version "2.4.21" apply false
}

// The toolchain pin (tools/build/toolchain.env), from mise's environment.
fun pin(name: String): String =
  providers.environmentVariable(name).orNull ?: error("$name is not set (run Gradle through mise)")

val androidHome = pin("ANDROID_HOME")
val compileSdk = pin("ANDROID_COMPILE_SDK").toInt()

allprojects {
  layout.buildDirectory = rootDir.resolve("../../out/build/system-app/$name").normalize()
}

subprojects {
  extra["compileSdk"] = compileSdk
  extra["buildTools"] = "$androidHome/build-tools/${pin("ANDROID_BUILD_TOOLS")}"
  // For the plain JVM modules. SDK 37 installs as platforms/android-37.0.
  extra["androidJar"] = "$androidHome/platforms/android-$compileSdk.0/android.jar"
}
