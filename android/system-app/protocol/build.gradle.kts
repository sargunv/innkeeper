// SPDX-License-Identifier: AGPL-3.0-or-later
// Pure Kotlin/JVM: the bridge's wire format, the input protocol, keymap and
// event translation. Android's constants come from the SDK's android.jar at
// compile time only (they are inlined), so the unit tests run on a plain JVM.
import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins { id("org.jetbrains.kotlin.jvm") }

val androidJar = extra["androidJar"] as String

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
  // Shared with the bridge's own tests (host/bridge/src/ipc/wire.rs).
  val wireFixtures = rootDir.resolve("../../host/bridge/src/ipc/wire-fixtures.txt").normalize()
  inputs.file(wireFixtures)
  systemProperty("wireFixtures", wireFixtures.path)
  val a11yFixture = rootDir.resolve("../../host/bridge/src/a11y/snapshot-fixture.txt").normalize()
  inputs.file(a11yFixture)
  systemProperty("a11yFixture", a11yFixture.path)
  testLogging { events("failed") }
}
