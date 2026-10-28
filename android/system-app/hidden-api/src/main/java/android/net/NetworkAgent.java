// SPDX-License-Identifier: AGPL-3.0-or-later
// Stub of a system API class (see android/system-app/hidden-api/build.gradle.kts).
package android.net;

import android.content.Context;
import android.os.Looper;

public abstract class NetworkAgent {
  public NetworkAgent(
      Context context,
      Looper looper,
      String logTag,
      NetworkCapabilities nc,
      LinkProperties lp,
      NetworkScore score,
      NetworkAgentConfig config,
      NetworkProvider provider) {
    throw new UnsupportedOperationException("stub");
  }

  public Network register() {
    throw new UnsupportedOperationException("stub");
  }

  public void markConnected() {
    throw new UnsupportedOperationException("stub");
  }

  public void sendLinkProperties(LinkProperties lp) {
    throw new UnsupportedOperationException("stub");
  }

  public void unregister() {
    throw new UnsupportedOperationException("stub");
  }
}
