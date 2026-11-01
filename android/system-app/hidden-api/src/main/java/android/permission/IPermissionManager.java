// SPDX-License-Identifier: AGPL-3.0-or-later
// Stub of an Android 17 platform API. It is compile-only and stays out of the APK.
package android.permission;

import android.os.Binder;
import android.os.IBinder;
import android.os.IInterface;
import android.os.RemoteException;

/** ServiceManager "permissionmgr". */
public interface IPermissionManager extends IInterface {
  int getPermissionFlags(
      String packageName, String permissionName, String persistentDeviceId, int userId)
      throws RemoteException;
  void grantRuntimePermission(
      String packageName, String permissionName, String persistentDeviceId, int userId)
      throws RemoteException;
  void revokeRuntimePermission(
      String packageName, String permissionName, String persistentDeviceId, int userId,
      String reason) throws RemoteException;

  abstract class Stub extends Binder implements IPermissionManager {
    public static IPermissionManager asInterface(IBinder binder) { throw new RuntimeException("stub"); }
    public IBinder asBinder() { return this; }
  }
}
