// SPDX-License-Identifier: AGPL-3.0-or-later
// Stub of an Android 17 platform API. It is compile-only and stays out of the APK.
package android.content.pm;

import android.content.IntentFilter;
import android.os.Binder;
import android.os.IBinder;
import android.os.IInterface;
import android.os.RemoteException;

/** ServiceManager "package". */
public interface IPackageManager extends IInterface {
  void deleteApplicationCacheFiles(String packageName, IPackageDataObserver observer)
      throws RemoteException;
  ParceledListSlice<IntentFilter> getAllIntentFilters(String packageName) throws RemoteException;

  abstract class Stub extends Binder implements IPackageManager {
    public static IPackageManager asInterface(IBinder binder) { throw new RuntimeException("stub"); }
    public IBinder asBinder() { return this; }
  }
}
