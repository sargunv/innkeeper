// SPDX-License-Identifier: AGPL-3.0-or-later
pluginManagement {
  repositories {
    google()
    mavenCentral()
    gradlePluginPortal()
  }
}

dependencyResolutionManagement {
  repositoriesMode = RepositoriesMode.FAIL_ON_PROJECT_REPOS
  repositories {
    google()
    mavenCentral()
  }
}

rootProject.name = "linuxapk-system"

include(":protocol", ":hidden-api", ":inject", ":app", ":probes")
