// SPDX-License-Identifier: AGPL-3.0-or-later
package android.view;

import android.graphics.Rect;

public final class InputWindowHandle {
  public String name;
  public Rect frame;
  public int displayId;
  public int ownerUid;
  public String packageName;
  public int layoutParamsFlags;
  public int layoutParamsType;
  public int inputConfig;
}
