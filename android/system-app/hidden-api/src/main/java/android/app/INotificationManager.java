// SPDX-License-Identifier: AGPL-3.0-or-later
// Stub of an Android 17 platform API. It is compile-only and stays out of the APK.
package android.app;

import android.content.ComponentName;
import android.os.Binder;
import android.os.IBinder;
import android.os.IInterface;
import android.os.RemoteException;

/** ServiceManager "notification". */
public interface INotificationManager extends IInterface {
  void setNotificationListenerAccessGranted(
      ComponentName listener, boolean enabled, boolean userSet) throws RemoteException;

  abstract class Stub extends Binder implements INotificationManager {
    public static INotificationManager asInterface(IBinder binder) { throw new RuntimeException("stub"); }
    public IBinder asBinder() { return this; }
  }
}
