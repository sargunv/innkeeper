// SPDX-License-Identifier: AGPL-3.0-or-later
// Compile-time stand-ins for the hidden framework classes and service interfaces the app calls
// (SystemProperties, the window, activity, package, permission, notification, input and power
// services, the network agent, the location provider). The app depends on this module
// compileOnly, so none of it ships in the APK: at runtime the boot classpath's real classes are
// used (hidden APIs are open to platform-signed apps). Signatures, return types included, match
// the pinned image's framework.jar. Hidden members of public SDK classes can't be stubbed here
// (android.jar's class wins), so those few calls stay reflective.
plugins { `java-library` }

val androidJar = extra["androidJar"] as String

java {
  sourceCompatibility = JavaVersion.VERSION_17
  targetCompatibility = JavaVersion.VERSION_17
}

dependencies { compileOnly(files(androidJar)) }
