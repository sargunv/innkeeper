// SPDX-License-Identifier: AGPL-3.0-or-later
// Stub of a hidden framework interface (see android/system-app/hidden-api/build.gradle.kts).
package android.view;

import android.os.RemoteException;

public interface IWindowManager extends android.os.IInterface {
  void setDisplayWindowInsetsController(int displayId, IDisplayWindowInsetsController controller)
      throws RemoteException;

  void setIgnoreOrientationRequest(int displayId, boolean ignore) throws RemoteException;

  void setDisplayImePolicy(int displayId, int imePolicy) throws RemoteException;
}
