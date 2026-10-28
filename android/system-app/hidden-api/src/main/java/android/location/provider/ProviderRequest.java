// SPDX-License-Identifier: AGPL-3.0-or-later
// Stub of a system API class (see android/system-app/hidden-api/build.gradle.kts).
package android.location.provider;

public final class ProviderRequest {
  public interface ChangedListener {
    void onProviderRequestChanged(String provider, ProviderRequest request);
  }

  public boolean isActive() {
    throw new UnsupportedOperationException("stub");
  }

  public long getIntervalMillis() {
    throw new UnsupportedOperationException("stub");
  }

  public int getQuality() {
    throw new UnsupportedOperationException("stub");
  }
}
