// SPDX-License-Identifier: AGPL-3.0-or-later
// Compile-time stand-ins for the hidden framework APIs the window manager uses
// (android.app.TaskStackListener, IActivityTaskManager, IWindowManager). The
// app depends on this module compileOnly, so none of it ships in the APK: at
// runtime the boot classpath's real classes are used (hidden APIs are open to
// platform-signed apps). Signatures match android-17.0.0_r1.
plugins { `java-library` }

val androidJar =
  providers
    .environmentVariable("ANDROID_HOME")
    .getOrElse("${System.getProperty("user.home")}/Android/Sdk") +
    "/platforms/android-37.0/android.jar"

java {
  sourceCompatibility = JavaVersion.VERSION_17
  targetCompatibility = JavaVersion.VERSION_17
}

dependencies { compileOnly(files(androidJar)) }
