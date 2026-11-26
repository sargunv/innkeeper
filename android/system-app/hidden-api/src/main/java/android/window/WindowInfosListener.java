// SPDX-License-Identifier: AGPL-3.0-or-later
package android.window;

import android.util.Pair;
import android.view.InputWindowHandle;

public abstract class WindowInfosListener {
  public abstract void onWindowInfosChanged(
      InputWindowHandle[] windowHandles, DisplayInfo[] displayInfos);

  public Pair<InputWindowHandle[], DisplayInfo[]> register() {
    throw new RuntimeException("Stub!");
  }

  public void unregister() {
    throw new RuntimeException("Stub!");
  }

  public static final class DisplayInfo {
    public int displayId;
  }
}
