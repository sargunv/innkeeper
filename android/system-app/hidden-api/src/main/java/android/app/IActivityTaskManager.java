// SPDX-License-Identifier: AGPL-3.0-or-later
// Stub of a hidden framework interface (see android/system-app/hidden-api/build.gradle.kts).
package android.app;

import android.os.RemoteException;
import java.util.List;

public interface IActivityTaskManager extends android.os.IInterface {
  void registerTaskStackListener(ITaskStackListener listener) throws RemoteException;

  void unregisterTaskStackListener(ITaskStackListener listener) throws RemoteException;

  List<ActivityManager.RunningTaskInfo> getTasks(
      int maxNum, boolean filterOnlyVisibleRecents, boolean keepIntentExtra, int displayId)
      throws RemoteException;

  boolean removeTask(int taskId) throws RemoteException;

  void moveRootTaskToDisplay(int taskId, int displayId) throws RemoteException;

  void focusTopTask(int displayId) throws RemoteException;
}
