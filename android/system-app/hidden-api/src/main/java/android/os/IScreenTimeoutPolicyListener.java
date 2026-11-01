// SPDX-License-Identifier: AGPL-3.0-or-later
// Stub of an Android 17 platform API. It is compile-only and stays out of the APK.
package android.os;

public interface IScreenTimeoutPolicyListener extends IInterface {
  void onScreenTimeoutPolicyChanged(int policy);

  abstract class Stub extends Binder implements IScreenTimeoutPolicyListener {
    public IBinder asBinder() { return this; }
  }
}
