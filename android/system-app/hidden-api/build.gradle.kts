// SPDX-License-Identifier: AGPL-3.0-or-later
// Compile-time stand-ins for the hidden framework APIs the window manager uses
// (android.app.TaskStackListener, IActivityTaskManager, IWindowManager) and
// the host network (android.net.NetworkAgent and its config types) and the
// location provider (android.location.provider.LocationProviderBase). The
// app depends on this module compileOnly, so none of it ships in the APK: at
// runtime the boot classpath's real classes are used (hidden APIs are open to
// platform-signed apps). Signatures match android-17.0.0_r1.
plugins { `java-library` }

val androidJar = extra["androidJar"] as String

java {
  sourceCompatibility = JavaVersion.VERSION_17
  targetCompatibility = JavaVersion.VERSION_17
}

dependencies { compileOnly(files(androidJar)) }
