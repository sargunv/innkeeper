// SPDX-License-Identifier: AGPL-3.0-or-later
// Android 17 platform API; compile only, never included in the APK.
package android.app;

import android.content.pm.IPackageDataObserver;
import android.os.Binder;
import android.os.IBinder;
import android.os.IInterface;
import android.os.RemoteException;

/** ServiceManager "activity". */
public interface IActivityManager extends IInterface {
  void forceStopPackage(String packageName, int userId) throws RemoteException;
  boolean clearApplicationUserData(
      String packageName, boolean keepState, IPackageDataObserver observer, int userId)
      throws RemoteException;

  abstract class Stub extends Binder implements IActivityManager {
    public static IActivityManager asInterface(IBinder binder) { throw new RuntimeException("stub"); }
    public IBinder asBinder() { return this; }
  }
}
