// SPDX-License-Identifier: AGPL-3.0-or-later
// Stub of a hidden framework interface (see android/hidden/build.gradle.kts).
package android.view;

import android.os.RemoteException;

public interface IWindowManager extends android.os.IInterface {
  void setDisplayImePolicy(int displayId, int imePolicy) throws RemoteException;
}
