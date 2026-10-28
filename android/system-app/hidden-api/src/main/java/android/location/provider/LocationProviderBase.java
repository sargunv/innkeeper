// SPDX-License-Identifier: AGPL-3.0-or-later
// Stub of a system API class (see android/system-app/hidden-api/build.gradle.kts).
package android.location.provider;

import android.content.Context;
import android.location.Location;
import android.os.Bundle;
import android.os.IBinder;

public abstract class LocationProviderBase {
  public interface OnFlushCompleteCallback {
    void onFlushComplete();
  }

  public static final String ACTION_NETWORK_PROVIDER =
      "com.android.location.service.v3.NetworkLocationProvider";

  public LocationProviderBase(Context context, String tag, ProviderProperties properties) {
    throw new UnsupportedOperationException("stub");
  }

  public final IBinder getBinder() {
    throw new UnsupportedOperationException("stub");
  }

  public void setAllowed(boolean allowed) {
    throw new UnsupportedOperationException("stub");
  }

  public boolean isAllowed() {
    throw new UnsupportedOperationException("stub");
  }

  public void reportLocation(Location location) {
    throw new UnsupportedOperationException("stub");
  }

  public abstract void onSetRequest(ProviderRequest request);

  public abstract void onFlush(OnFlushCompleteCallback callback);

  public abstract void onSendExtraCommand(String command, Bundle extras);
}
