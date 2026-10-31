// SPDX-License-Identifier: AGPL-3.0-or-later
// Android 17 platform API; compile only, never included in the APK.
package android.hardware.input;

import android.os.Binder;
import android.os.IBinder;
import android.os.IInterface;
import android.os.RemoteException;

/** ServiceManager "input". */
public interface IInputManager extends IInterface {
  void addUniqueIdAssociationByPort(String inputPort, String displayUniqueId) throws RemoteException;
  void removeUniqueIdAssociationByPort(String inputPort) throws RemoteException;
  void setMouseScalingEnabled(boolean enabled, int displayId) throws RemoteException;

  abstract class Stub extends Binder implements IInputManager {
    public static IInputManager asInterface(IBinder binder) { throw new RuntimeException("stub"); }
    public IBinder asBinder() { return this; }
  }
}
