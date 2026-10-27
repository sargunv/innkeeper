// SPDX-License-Identifier: AGPL-3.0-or-later
// Pure Kotlin/JVM: the input protocol, keymap and event translation. Android's
// constants come from the SDK's android.jar at compile time only (they are
// inlined), so the unit tests run on a plain JVM.
import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins { id("org.jetbrains.kotlin.jvm") }

val androidJar =
  providers
    .environmentVariable("ANDROID_HOME")
    .getOrElse("${System.getProperty("user.home")}/Android/Sdk") +
    "/platforms/android-37.0/android.jar"

java {
  sourceCompatibility = JavaVersion.VERSION_17
  targetCompatibility = JavaVersion.VERSION_17
}

kotlin { compilerOptions { jvmTarget = JvmTarget.JVM_17 } }

dependencies {
  compileOnly(files(androidJar))
  testCompileOnly(files(androidJar))
  testImplementation(kotlin("test"))
}

tasks.test {
  useJUnitPlatform()
  testLogging { events("failed") }
}
