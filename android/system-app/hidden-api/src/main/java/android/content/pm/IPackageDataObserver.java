// SPDX-License-Identifier: AGPL-3.0-or-later
package android.content.pm;

import android.os.Binder;
import android.os.IBinder;
import android.os.IInterface;
import android.os.Parcel;
import android.os.RemoteException;

/** Compile-only signature of the framework's package-data callback. */
public interface IPackageDataObserver extends IInterface {
  void onRemoveCompleted(String packageName, boolean succeeded) throws RemoteException;

  abstract class Stub extends Binder implements IPackageDataObserver {
    public Stub() { attachInterface(this, "android.content.pm.IPackageDataObserver"); }
    @Override public IBinder asBinder() { return this; }
    @Override public boolean onTransact(int code, Parcel data, Parcel reply, int flags)
        throws RemoteException { throw new UnsupportedOperationException("stub"); }
  }
}
