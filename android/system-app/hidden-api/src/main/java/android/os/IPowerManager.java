// SPDX-License-Identifier: AGPL-3.0-or-later
// Android 17 platform API; compile only, never included in the APK.
package android.os;

public interface IPowerManager extends IInterface {
  void addScreenTimeoutPolicyListener(int displayId, IScreenTimeoutPolicyListener listener) throws RemoteException;
  void removeScreenTimeoutPolicyListener(int displayId, IScreenTimeoutPolicyListener listener) throws RemoteException;
  void goToSleepWithDisplayId(int displayId, long time, int reason, int flags) throws RemoteException;
  void wakeUpWithDisplayId(long time, int reason, String details, String packageName, int displayId)
      throws RemoteException;

  abstract class Stub extends Binder implements IPowerManager {
    public static IPowerManager asInterface(IBinder binder) { throw new RuntimeException("stub"); }
    public IBinder asBinder() { return this; }
  }
}
