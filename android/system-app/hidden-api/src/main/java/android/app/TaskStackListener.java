// SPDX-License-Identifier: AGPL-3.0-or-later
// Stub of a hidden framework class (see
// android/system-app/hidden-api/build.gradle.kts). Only the callbacks the
// window manager overrides are declared.
package android.app;

import android.content.ComponentName;
import android.os.RemoteException;

public abstract class TaskStackListener extends ITaskStackListener.Stub {
  public void onTaskStackChanged() throws RemoteException {}

  public void onTaskCreated(int taskId, ComponentName componentName) throws RemoteException {}

  public void onTaskRemoved(int taskId) throws RemoteException {}

  public void onTaskMovedToFront(ActivityManager.RunningTaskInfo taskInfo)
      throws RemoteException {}

  public void onTaskDescriptionChanged(ActivityManager.RunningTaskInfo taskInfo)
      throws RemoteException {}

  public void onTaskDisplayChanged(int taskId, int newDisplayId) throws RemoteException {}

  public void onTaskFocusChanged(int taskId, boolean focused) {}

  public void onTaskMovedToBack(ActivityManager.RunningTaskInfo taskInfo) {}
}
