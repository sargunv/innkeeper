// SPDX-License-Identifier: AGPL-3.0-or-later
// Android 17 platform API; compile only, never included in the APK.
package android.os;

public interface IScreenTimeoutPolicyListener extends IInterface {
  void onScreenTimeoutPolicyChanged(int policy);

  abstract class Stub extends Binder implements IScreenTimeoutPolicyListener {
    public IBinder asBinder() { return this; }
  }
}
